//! Graph mutation commands — Phase 3 of per-card divergence,
//! generalized to support both effect graphs and generator graphs.
//!
//! Each command operates on the `EffectGraphDef` that a
//! [`manifold_core::GraphTarget`] points at. Targets resolve to:
//!
//! - [`GraphTarget::Effect`] → [`EffectInstance::graph`] with
//!   `EffectInstance::graph_version` as the version counter.
//! - [`GraphTarget::Generator`] → [`crate::commands::graph::Layer::generator_graph`]
//!   (via `Project::timeline::find_layer_by_id_mut`) with
//!   `Layer::generator_graph_version` as the version counter.
//!
//! Commands lift a `None` graph to a clone of the supplied catalog
//! default on first edit, apply the mutation, then bump the target's
//! version counter so the renderer detects the change. Reverse state
//! for undo/redo is stored on each command instance.
//!
//! Phase 3 of the per-card-divergence plan in
//! `docs/NODE_GRAPH_SYSTEM.md`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::project::Project;

use crate::command::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a [`GraphTarget`] to a mutable [`EffectGraphDef`] inside
/// `project`, lifting a `None` graph to a clone of `catalog_default`
/// on first edit. Runs `f` against the def, then bumps the target's
/// version counter so the renderer notices the change.
///
/// Returns `Some(R)` from `f`, or `None` if the target no longer
/// resolves (effect / layer was deleted between command creation and
/// execution — both possible across undo/redo cycles).
fn with_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    match target {
        GraphTarget::Effect(eid) => {
            let inst = project.find_effect_by_id_mut(eid)?;
            let def = inst.graph.get_or_insert_with(|| catalog_default.clone());
            let r = f(def);
            inst.graph_version = inst.graph_version.wrapping_add(1);
            Some(r)
        }
        GraphTarget::Generator(lid) => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(lid)?;
            let def = layer
                .generator_graph
                .get_or_insert_with(|| catalog_default.clone());
            let r = f(def);
            layer.generator_graph_version = layer.generator_graph_version.wrapping_add(1);
            Some(r)
        }
    }
}

/// Variant of [`with_target_graph_mut`] that doesn't lift the graph
/// from `None` — `f` only runs if the target already has a `Some(def)`.
/// Used by undo paths that mutate an already-edited graph; the catalog
/// default isn't needed because if the graph is `None` there's nothing
/// to undo.
fn with_existing_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    match target {
        GraphTarget::Effect(eid) => {
            let inst = project.find_effect_by_id_mut(eid)?;
            let def = inst.graph.as_mut()?;
            let r = f(def);
            inst.graph_version = inst.graph_version.wrapping_add(1);
            Some(r)
        }
        GraphTarget::Generator(lid) => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(lid)?;
            let def = layer.generator_graph.as_mut()?;
            let r = f(def);
            layer.generator_graph_version = layer.generator_graph_version.wrapping_add(1);
            Some(r)
        }
    }
}

/// Helper for the Revert command: take the target's current
/// `Option<EffectGraphDef>` (consuming it; leaves `None` in place) and
/// return what was there. Bumps the version counter.
fn take_target_graph(
    project: &mut Project,
    target: &GraphTarget,
) -> Option<Option<EffectGraphDef>> {
    match target {
        GraphTarget::Effect(eid) => {
            let inst = project.find_effect_by_id_mut(eid)?;
            let prev = inst.graph.take();
            inst.graph_version = inst.graph_version.wrapping_add(1);
            Some(prev)
        }
        GraphTarget::Generator(lid) => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(lid)?;
            let prev = layer.generator_graph.take();
            layer.generator_graph_version = layer.generator_graph_version.wrapping_add(1);
            Some(prev)
        }
    }
}

/// Helper for the Revert command: install a given graph (or `None`)
/// at the target, bumping the version counter.
fn install_target_graph(
    project: &mut Project,
    target: &GraphTarget,
    graph: Option<EffectGraphDef>,
) {
    match target {
        GraphTarget::Effect(eid) => {
            if let Some(inst) = project.find_effect_by_id_mut(eid) {
                inst.graph = graph;
                inst.graph_version = inst.graph_version.wrapping_add(1);
            }
        }
        GraphTarget::Generator(lid) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                layer.generator_graph = graph;
                layer.generator_graph_version =
                    layer.generator_graph_version.wrapping_add(1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Add Graph Node
// ---------------------------------------------------------------------------

/// Add a new node to the per-card graph at the given editor position.
/// The new node has default parameters and no port wires until a
/// subsequent [`ConnectPortsCommand`] connects it.
#[derive(Debug)]
pub struct AddGraphNodeCommand {
    target: GraphTarget,
    node_type_id: String,
    pos: Option<(f32, f32)>,
    catalog_default: EffectGraphDef,
    /// `id` minted at first execute. Persisted across undo/redo so
    /// re-execute reuses the same id — downstream commands
    /// (`ConnectPorts`, `SetGraphNodeParam`) address by id.
    minted_id: Option<u32>,
}

impl AddGraphNodeCommand {
    pub fn new(
        target: GraphTarget,
        node_type_id: String,
        pos: Option<(f32, f32)>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            node_type_id,
            pos,
            catalog_default,
            minted_id: None,
        }
    }

    /// Id assigned to the newly-added node on first execute. `None`
    /// until `execute` runs successfully.
    pub fn new_node_id(&self) -> Option<u32> {
        self.minted_id
    }
}

impl Command for AddGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_type_id = self.node_type_id.clone();
        let pos = self.pos;
        let prev_minted = self.minted_id;
        let minted = with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
            let next_id = def
                .nodes
                .iter()
                .map(|n| n.id)
                .max()
                .map_or(0, |m| m + 1);
            let id = prev_minted.unwrap_or(next_id);
            def.nodes.push(EffectGraphNode {
                id,
                type_id: node_type_id,
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: pos,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
            });
            id
        });
        match minted {
            Some(id) => self.minted_id = Some(id),
            None => eprintln!(
                "[manifold-editing] AddGraphNode: target {} did not resolve",
                self.target.label()
            ),
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(id) = self.minted_id else { return };
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            def.nodes.retain(|n| n.id != id);
            def.wires.retain(|w| w.from_node != id && w.to_node != id);
        });
    }

    fn description(&self) -> &str {
        "Add Graph Node"
    }
}

// ---------------------------------------------------------------------------
// Remove Graph Node
// ---------------------------------------------------------------------------

/// Remove a node from the per-card graph plus every wire touching it.
/// Both the node and the disconnected wires are stashed for undo.
#[derive(Debug)]
pub struct RemoveGraphNodeCommand {
    target: GraphTarget,
    node_id: u32,
    catalog_default: EffectGraphDef,
    /// Reverse state. `None` before first execute; populated to the
    /// removed node + its incident wires on success.
    removed: Option<RemovedNode>,
}

#[derive(Debug, Clone)]
struct RemovedNode {
    node: EffectGraphNode,
    wires: Vec<EffectGraphWire>,
}

impl RemoveGraphNodeCommand {
    pub fn new(target: GraphTarget, node_id: u32, catalog_default: EffectGraphDef) -> Self {
        Self {
            target,
            node_id,
            catalog_default,
            removed: None,
        }
    }
}

impl Command for RemoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        let removed =
            with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
                let node_pos = def.nodes.iter().position(|n| n.id == node_id)?;
                let node = def.nodes.remove(node_pos);
                let removed_wires: Vec<EffectGraphWire> = def
                    .wires
                    .iter()
                    .filter(|w| w.from_node == node_id || w.to_node == node_id)
                    .cloned()
                    .collect();
                def.wires
                    .retain(|w| w.from_node != node_id && w.to_node != node_id);
                Some(RemovedNode {
                    node,
                    wires: removed_wires,
                })
            })
            .flatten();
        if removed.is_some() {
            self.removed = removed;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(removed) = self.removed.clone() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            def.nodes.push(removed.node);
            def.wires.extend(removed.wires);
        });
    }

    fn description(&self) -> &str {
        "Remove Graph Node"
    }
}

// ---------------------------------------------------------------------------
// Connect Ports
// ---------------------------------------------------------------------------

/// Add a wire from one node's output port to another node's input
/// port. Inputs accept exactly one source, so any wire already
/// targeting `(to_node, to_port)` is replaced and stashed for undo
/// (same semantics as the runtime [`Graph::connect`]).
#[derive(Debug)]
pub struct ConnectPortsCommand {
    target: GraphTarget,
    from_node: u32,
    from_port: String,
    to_node: u32,
    to_port: String,
    catalog_default: EffectGraphDef,
    /// Wire that previously fed `(to_node, to_port)`, if any.
    /// Restored by undo before the new wire is removed.
    displaced: Option<EffectGraphWire>,
}

impl ConnectPortsCommand {
    pub fn new(
        target: GraphTarget,
        from_node: u32,
        from_port: String,
        to_node: u32,
        to_port: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            from_node,
            from_port,
            to_node,
            to_port,
            catalog_default,
            displaced: None,
        }
    }
}

impl Command for ConnectPortsCommand {
    fn execute(&mut self, project: &mut Project) {
        let from_node = self.from_node;
        let from_port = self.from_port.clone();
        let to_node = self.to_node;
        let to_port = self.to_port.clone();
        let displaced =
            with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
                let displaced = def
                    .wires
                    .iter()
                    .position(|w| w.to_node == to_node && w.to_port == to_port)
                    .map(|i| def.wires.remove(i));
                def.wires.push(EffectGraphWire {
                    from_node,
                    from_port,
                    to_node,
                    to_port,
                });
                displaced
            })
            .flatten();
        self.displaced = displaced;
    }

    fn undo(&mut self, project: &mut Project) {
        let from_node = self.from_node;
        let from_port = self.from_port.clone();
        let to_node = self.to_node;
        let to_port = self.to_port.clone();
        let displaced = self.displaced.take();
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            if let Some(pos) = def.wires.iter().position(|w| {
                w.from_node == from_node
                    && w.from_port == from_port
                    && w.to_node == to_node
                    && w.to_port == to_port
            }) {
                def.wires.remove(pos);
            }
            if let Some(wire) = displaced {
                def.wires.push(wire);
            }
        });
    }

    fn description(&self) -> &str {
        "Connect Ports"
    }
}

// ---------------------------------------------------------------------------
// Disconnect Ports
// ---------------------------------------------------------------------------

/// Remove whatever wire feeds the given input port. Idempotent — a
/// disconnect on an unwired port stashes `None` for undo and no-ops.
#[derive(Debug)]
pub struct DisconnectPortsCommand {
    target: GraphTarget,
    to_node: u32,
    to_port: String,
    catalog_default: EffectGraphDef,
    /// The wire we removed, restored by undo.
    removed: Option<EffectGraphWire>,
}

impl DisconnectPortsCommand {
    pub fn new(
        target: GraphTarget,
        to_node: u32,
        to_port: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            to_node,
            to_port,
            catalog_default,
            removed: None,
        }
    }
}

impl Command for DisconnectPortsCommand {
    fn execute(&mut self, project: &mut Project) {
        let to_node = self.to_node;
        let to_port = self.to_port.clone();
        let removed = with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
            def.wires
                .iter()
                .position(|w| w.to_node == to_node && w.to_port == to_port)
                .map(|pos| def.wires.remove(pos))
        })
        .flatten();
        self.removed = removed;
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(wire) = self.removed.take() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            def.wires.push(wire);
        });
    }

    fn description(&self) -> &str {
        "Disconnect Ports"
    }
}

// ---------------------------------------------------------------------------
// Move Graph Node
// ---------------------------------------------------------------------------

/// Update a node's editor position. Doesn't affect runtime behaviour —
/// `editor_pos` is purely a canvas layout hint — but does bump
/// `graph_version` so the snapshot pipeline sees the new position.
#[derive(Debug)]
pub struct MoveGraphNodeCommand {
    target: GraphTarget,
    node_id: u32,
    new_pos: (f32, f32),
    catalog_default: EffectGraphDef,
    /// Position before execute(), for undo.
    previous_pos: Option<Option<(f32, f32)>>,
}

impl MoveGraphNodeCommand {
    pub fn new(
        target: GraphTarget,
        node_id: u32,
        new_pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            node_id,
            new_pos,
            catalog_default,
            previous_pos: None,
        }
    }
}

impl Command for MoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        let new_pos = self.new_pos;
        let prev_already_captured = self.previous_pos.is_some();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
                let node = def.nodes.iter_mut().find(|n| n.id == node_id)?;
                let prev = node.editor_pos;
                node.editor_pos = Some(new_pos);
                Some(prev)
            })
            .flatten();
        if !prev_already_captured && let Some(prev) = captured {
            self.previous_pos = Some(prev);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous) = self.previous_pos else {
            return;
        };
        let node_id = self.node_id;
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            if let Some(node) = def.nodes.iter_mut().find(|n| n.id == node_id) {
                node.editor_pos = previous;
            }
        });
    }

    fn description(&self) -> &str {
        "Move Graph Node"
    }
}

// ---------------------------------------------------------------------------
// Set Graph Node Param
// ---------------------------------------------------------------------------

/// Set a single inner-node parameter on the per-card graph. The
/// previous value (or absence) is stashed for undo. Tagged-enum
/// [`SerializedParamValue`] lets the command carry every primitive
/// param type without a renderer-side dependency.
#[derive(Debug)]
pub struct SetGraphNodeParamCommand {
    target: GraphTarget,
    node_id: u32,
    param_name: String,
    new_value: SerializedParamValue,
    catalog_default: EffectGraphDef,
    /// Value before execute(). `Some(None)` means "key was absent";
    /// `Some(Some(v))` means "key existed with value `v`". `None` at
    /// pre-execute time.
    previous_value: Option<Option<SerializedParamValue>>,
}

impl SetGraphNodeParamCommand {
    pub fn new(
        target: GraphTarget,
        node_id: u32,
        param_name: String,
        new_value: SerializedParamValue,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            node_id,
            param_name,
            new_value,
            catalog_default,
            previous_value: None,
        }
    }
}

impl Command for SetGraphNodeParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        let param_name = self.param_name.clone();
        let new_value = self.new_value.clone();
        let prev_already_captured = self.previous_value.is_some();
        // Closure return: `Option<SerializedParamValue>` — None if the
        // key didn't exist before the insert, Some(prev) if it did.
        // `with_target_graph_mut` wraps in another Option for target
        // resolution. `.flatten()` collapses: `None` here means either
        // the target didn't resolve OR the node id wasn't in the graph.
        let captured: Option<Option<SerializedParamValue>> =
            with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
                def.nodes
                    .iter_mut()
                    .find(|n| n.id == node_id)
                    .map(|node| node.params.insert(param_name, new_value))
            })
            .flatten();
        if !prev_already_captured && let Some(prev) = captured {
            // `prev: Option<SerializedParamValue>` — distinguishes
            // "key was absent" from "key existed with value `v`". Stored
            // as `Some(prev)` so undo knows we successfully captured.
            self.previous_value = Some(prev);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous_value.take() else {
            return;
        };
        let node_id = self.node_id;
        let param_name = self.param_name.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            if let Some(node) = def.nodes.iter_mut().find(|n| n.id == node_id) {
                match prev {
                    Some(v) => {
                        node.params.insert(param_name, v);
                    }
                    None => {
                        node.params.remove(&param_name);
                    }
                }
            }
        });
    }

    fn description(&self) -> &str {
        "Set Graph Node Param"
    }
}

// ---------------------------------------------------------------------------
// Revert Graph (effect or generator)
// ---------------------------------------------------------------------------

/// Clear the per-target graph override (either an `EffectInstance::graph`
/// or a `Layer::generator_graph`), reverting to the bundled preset.
/// The next chain rebuild reads the catalog default instead of the
/// saved-in-place graph.
///
/// Idempotent on execute: if the override is already `None`, the
/// command stores `None` for undo and does nothing else. On undo,
/// restores the previous `Some(def)` if there was one.
///
/// The "library picker" surfaces this command as the user-facing
/// "Reset to Default Preset" action on a diverged effect or generator.
#[derive(Debug)]
pub struct RevertEffectGraphCommand {
    target: GraphTarget,
    /// Pre-execute snapshot of the target's `graph`. `None` if the
    /// effect/generator was already on the catalog default, `Some(def)`
    /// if it had an override that this command cleared.
    previous: Option<Option<EffectGraphDef>>,
}

impl RevertEffectGraphCommand {
    pub fn new(target: GraphTarget) -> Self {
        Self {
            target,
            previous: None,
        }
    }
}

impl Command for RevertEffectGraphCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.previous.is_none() {
            // First execute: capture and clear.
            self.previous = take_target_graph(project, &self.target);
        } else {
            // Re-execute (after undo): clear without re-capturing.
            install_target_graph(project, &self.target, None);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous.take() else {
            return;
        };
        install_target_graph(project, &self.target, prev);
    }

    fn description(&self) -> &str {
        "Revert Graph"
    }
}

// ---------------------------------------------------------------------------
// Toggle Node Param Expose (unified Effect + Generator)
// ---------------------------------------------------------------------------

/// Toggle whether an inner-graph parameter is exposed on the outer
/// card. **Single command for both Effect-hosted and Generator-hosted
/// graphs** — the graph editor is one surface, the click handler emits
/// one [`crate::PanelAction`], and exposure state lives in one place
/// (the graph node's `exposed_params` set).
///
/// For Effect targets, this command also mirrors the new state into
/// the legacy `EffectInstance.param_values[i].exposed` (for params
/// covered by a preset binding's static-block slot) and
/// [`EffectInstance::user_param_bindings`] (for inner-node params with
/// no preset binding). The mirror is what keeps the timeline-card
/// state-sync path working until Step 8 of the unification cuts those
/// fields over to the graph as the single source of truth.
///
/// For Generator targets, only the graph write happens — generators
/// never had a legacy `param_values` shadow.
#[derive(Debug)]
pub struct ToggleNodeParamExposeCommand {
    target: GraphTarget,
    node_handle: String,
    inner_param: String,
    expose: bool,
    catalog_default: EffectGraphDef,
    /// Inner-node ParamDef metadata captured at panel-build time.
    /// Required when the Effect-side mirror needs to append a new
    /// `UserParamBinding` — the binding needs label/min/max/default/
    /// convert to be well-formed. Generators ignore this.
    inner_meta: Option<manifold_core::effects::ParamConvert>,
    /// Angle presentation hint for the inner param, captured at panel-build
    /// time from `ParamType::Angle`. Flows onto the appended
    /// `UserParamBinding` so the card slider shows degrees. Display-only —
    /// storage stays radians.
    inner_is_angle: bool,
    /// Display label for the user binding (effect-side only).
    inner_label: String,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    /// Reverse state, populated on first execute(). See
    /// [`NodeExposeReverse`].
    reverse: NodeExposeReverse,
}

// Two-variant undo-state enum: `None` until execute() runs, then
// `Captured` carries everything needed to reverse the toggle. The
// `Captured` variant grew past the clippy size threshold when the
// envelope-cleanup work landed, but boxing the captured payload
// would add heap traffic to every undoable graph-toggle command
// for no real win — these structs live in an undo stack capped at
// 200 entries, not on any hot path. Lint suppressed deliberately.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Default)]
enum NodeExposeReverse {
    #[default]
    None,
    /// Captured on execute. Restored on undo.
    Captured {
        /// Previous membership of `inner_param` in the node's
        /// `exposed_params` set. `true` if it was present before
        /// execute, `false` otherwise. Restored unconditionally on undo.
        prev_in_set: bool,
        /// Per-target mirror reverse state. Exactly one of the two
        /// inner variants ever populates per execute — the dispatch
        /// is keyed off the command's `target` field, not stored on
        /// the reverse state.
        mirror: MirrorReverse,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum MirrorReverse {
    /// Effect target. Restored via `unmirror_effect_side`.
    Effect(EffectMirrorReverse),
    /// Generator target. Restored via `unmirror_generator_side`.
    Generator(GeneratorMirrorReverse),
}

// `RemovedUserBinding` is large because it captures the full
// `UserParamBinding` + every orphaned driver / Ableton mapping /
// envelope so undo can faithfully restore the pre-unexpose state.
// Boxing it would only shrink the enum footprint on the undo
// stack — the captured payload lives there for at most ~200
// commands and is never on a render hot path, so the indirection
// trade isn't worth it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum EffectMirrorReverse {
    /// The (handle, param) maps to a static-block slot; we flipped
    /// `param_values[slot].exposed`. Undo restores `prev_exposed`.
    StaticSlot { slot: usize, prev_exposed: bool },
    /// The (handle, param) is a non-preset param; we appended a
    /// `UserParamBinding`. Undo removes it by id.
    AppendedUserBinding {
        user_param_id: String,
    },
    /// The (handle, param) is a non-preset param; we removed an
    /// existing `UserParamBinding`. Undo reinserts it at `position`
    /// with the captured slot value, plus re-attaches any orphaned
    /// drivers / Ableton mappings / envelopes that referenced the
    /// binding's id.
    RemovedUserBinding {
        binding: manifold_core::effects::UserParamBinding,
        position: usize,
        slot_value: manifold_core::effects::ParamSlot,
        /// Drivers pruned from `EffectInstance.drivers` because their
        /// `param_id` matched the removed binding's id. Without this
        /// pruning the rows would survive in the project file but
        /// never resolve to a target, leaving silently-dead
        /// automation behind.
        removed_drivers: Vec<manifold_core::effects::ParameterDriver>,
        /// Ableton mappings pruned for the same reason.
        removed_ableton_mappings:
            Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
        /// Envelopes pruned from the host layer's envelope list when the
        /// removed binding's (effect_type, param_id) matched. `None`
        /// when the effect is master-scoped (master has no envelopes)
        /// or clip-scoped (clip-hosted effects don't surface in the
        /// graph editor today). When `Some`, `layer_id` is the host
        /// layer — undo restores the captured envelopes there.
        removed_envelope_state: Option<RemovedEnvelopeState>,
    },
    /// No-op: the Effect-side state already matched the requested
    /// state (idempotent re-toggle). Nothing to undo on the mirror.
    NoOp,
}

/// Generator-side reverse state for `ToggleNodeParamExposeCommand`.
/// Generators (approach A in `docs/EFFECT_GENERATOR_CARD_UNIFICATION.md`)
/// store user-added binding metadata in the **graph itself** — new
/// entries get appended to `preset_metadata.params` + `bindings`, and
/// `gp.param_values` grows by one slot. There's no parallel
/// `Vec<UserParamBinding>` on the host like effects use; the graph is
/// the single source of truth for everything except the per-frame
/// value bus.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum GeneratorMirrorReverse {
    /// Exposure toggle covered an existing (bundled or already-added)
    /// binding — nothing to do on the metadata side beyond the
    /// `exposed_params` flip handled at the outer level. Slot value
    /// stays.
    NoOp,
    /// Appended a new user-added binding. Undo removes the
    /// `BindingDef` + `ParamSpecDef` pair (both freshly appended at
    /// the tail) and pops the matching `gp.param_values` slot.
    AppendedUserBinding {
        /// The id we minted for this binding. Captured for `Debug`
        /// formatting and test assertions; undo removes by positional
        /// index rather than by id, so the field is not read during
        /// `unmirror_generator_side`.
        #[allow(dead_code)]
        user_param_id: String,
        /// Tail index where the new `BindingDef` was inserted. Always
        /// `bindings.len() - 1` immediately after append, captured for
        /// robustness against intervening edits before undo.
        binding_index: usize,
        /// Tail index where the new `ParamSpecDef` was inserted.
        spec_index: usize,
        /// Slot index in `gp.param_values` where the new value lives.
        /// `spec_index` and `slot_index` track the same position — both
        /// captured for assertion-friendly undo restoration.
        slot_index: usize,
    },
    /// Removed a user-added binding (user_added=true). Undo
    /// reinserts the BindingDef + ParamSpecDef at the captured
    /// positions, restores the slot value at `slot_index`, and
    /// re-attaches any orphaned automation rows.
    RemovedUserBinding {
        binding: manifold_core::effect_graph_def::BindingDef,
        spec: manifold_core::effect_graph_def::ParamSpecDef,
        binding_index: usize,
        spec_index: usize,
        slot_index: usize,
        slot_value: f32,
        /// Drivers pruned from `gp.drivers` because their `param_id`
        /// matched the removed binding's id.
        removed_drivers: Vec<manifold_core::effects::ParameterDriver>,
        /// Envelopes pruned from `gp.envelopes`. Generators store
        /// envelopes directly on `GeneratorParamState` (not on the
        /// host layer like effects do), so capturing them needs only
        /// the same borrow as the rest of the gen-mirror cleanup.
        removed_envelopes: Vec<manifold_core::effects::ParamEnvelope>,
        /// Ableton mappings pruned from `gp.ableton_mappings`.
        removed_ableton_mappings:
            Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
    },
}

/// Captured envelope orphans from a Layer-hosted effect's unexpose.
/// Envelopes live on the [`manifold_core::layer::Layer`] (keyed by
/// `(target_effect_type, param_id)`), not on the [`EffectInstance`],
/// so capturing them needs the layer borrow — separate from the rest
/// of `EffectMirrorReverse` which only needs the effect borrow.
#[derive(Debug)]
struct RemovedEnvelopeState {
    /// Host layer the envelopes belong to. Undo restores them here.
    layer_id: manifold_core::LayerId,
    /// Effect type id captured at unexpose time. Envelopes match on
    /// `(target_effect_type, param_id)`, so we need the type alongside
    /// the binding id to put them back at the right rows.
    effect_type: manifold_core::EffectTypeId,
    /// The pruned envelope rows, in the order they appeared on the
    /// layer. `retain` doesn't preserve indices, so the restore path
    /// appends them — fine because envelopes are keyed by content
    /// `(effect_type, param_id)`, not position.
    envelopes: Vec<manifold_core::effects::ParamEnvelope>,
}

impl ToggleNodeParamExposeCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        node_handle: String,
        inner_param: String,
        expose: bool,
        catalog_default: EffectGraphDef,
        inner_label: String,
        inner_min: f32,
        inner_max: f32,
        inner_default: f32,
        inner_convert: manifold_core::effects::ParamConvert,
        inner_is_angle: bool,
    ) -> Self {
        Self {
            target,
            node_handle,
            inner_param,
            expose,
            catalog_default,
            inner_meta: Some(inner_convert),
            inner_is_angle,
            inner_label,
            inner_min,
            inner_max,
            inner_default,
            reverse: NodeExposeReverse::None,
        }
    }
}

/// Flip `inner_param` membership in the matching `EffectGraphNode`'s
/// `exposed_params` set. Returns the previous membership for undo.
/// `None` if the def has no node with that handle.
fn flip_graph_exposed(
    def: &mut EffectGraphDef,
    node_handle: &str,
    inner_param: &str,
    expose: bool,
) -> Option<bool> {
    // First materialise the preset-binding-driven exposure defaults
    // into the def. This converts the def from "implicit (use preset
    // bindings as the default exposure set)" into "explicit (the set
    // IS the exposure state)". After this call, absence from
    // `exposed_params` means "user explicitly unset" — which is what
    // the persistence + snapshot path needs for unchecks to stick.
    // Idempotent: re-running adds nothing new because the bindings
    // map to the same handle/param pairs every time.
    materialize_binding_exposures(def);

    let node = def
        .nodes
        .iter_mut()
        .find(|n| n.handle.as_deref() == Some(node_handle))?;
    let was = node.exposed_params.contains(inner_param);
    if expose {
        node.exposed_params.insert(inner_param.to_string());
    } else {
        node.exposed_params.remove(inner_param);
    }
    Some(was)
}

/// Walk every binding in `def.preset_metadata.bindings` and ensure
/// the matching node's `exposed_params` set contains the target param.
/// Used by [`flip_graph_exposed`] to materialise the implicit
/// preset-driven defaults before applying a user toggle. After the
/// first materialisation, `into_graph`'s binding backfill becomes a
/// no-op (it short-circuits when the def already carries explicit
/// exposure entries), so unchecks stick across save/reload.
fn materialize_binding_exposures(def: &mut EffectGraphDef) {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(meta) = def.preset_metadata.as_ref() else {
        return;
    };
    // Collect the (handle, param) pairs first; we can't borrow meta
    // immutably while mutating nodes.
    let pairs: Vec<(String, String)> = meta
        .bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::HandleNode { handle, param } => {
                Some((handle.clone(), param.clone()))
            }
            BindingTarget::Composite { .. } => None,
        })
        .collect();
    for (handle, param) in pairs {
        if let Some(node) = def
            .nodes
            .iter_mut()
            .find(|n| n.handle.as_deref() == Some(handle.as_str()))
        {
            node.exposed_params.insert(param);
        }
    }
}

/// Restore `inner_param` membership in the matching node's
/// `exposed_params` set to `prev_in_set`. Idempotent — silently no-ops
/// if the node is gone.
fn restore_graph_exposed(
    def: &mut EffectGraphDef,
    node_handle: &str,
    inner_param: &str,
    prev_in_set: bool,
) {
    if let Some(node) = def
        .nodes
        .iter_mut()
        .find(|n| n.handle.as_deref() == Some(node_handle))
    {
        if prev_in_set {
            node.exposed_params.insert(inner_param.to_string());
        } else {
            node.exposed_params.remove(inner_param);
        }
    }
}

/// Find the static-block param slot index for an (inner_node_handle,
/// inner_param) pair, by scanning the preset metadata's bindings.
/// Returns the position in `metadata.params` of the binding whose
/// target is `(handle, param)`. `None` if the def has no metadata or
/// no binding targets that (handle, param).
fn static_slot_for(
    def: &EffectGraphDef,
    node_handle: &str,
    inner_param: &str,
) -> Option<usize> {
    use manifold_core::effect_graph_def::BindingTarget;
    let meta = def.preset_metadata.as_ref()?;
    let binding_idx = meta.bindings.iter().position(|b| match &b.target {
        BindingTarget::HandleNode { handle, param } => {
            handle == node_handle && param == inner_param
        }
        BindingTarget::Composite { .. } => false,
    })?;
    // Static-block slots are positional against `metadata.params` —
    // each `params[i]` corresponds to bindings sharing the same `id`.
    let binding_id = &meta.bindings[binding_idx].id;
    meta.params.iter().position(|p| &p.id == binding_id)
}

impl Command for ToggleNodeParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let inner_label = self.inner_label.clone();
        let inner_min = self.inner_min;
        let inner_max = self.inner_max;
        let inner_default = self.inner_default;
        let inner_convert = self.inner_meta.unwrap_or(manifold_core::effects::ParamConvert::Float);
        let inner_is_angle = self.inner_is_angle;

        // Graph-side write — for Effect targets, capture the
        // static-block slot so the effect-side mirror knows whether
        // to flip an existing `param_values[i].exposed` or append a
        // user binding. For Generator targets, capture the spec for
        // the user-added append/remove path directly on the graph.
        let graph_result = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            |def| {
                let prev_in_set = flip_graph_exposed(def, &node_handle, &inner_param, expose)
                    .unwrap_or(false);
                let static_slot = static_slot_for(def, &node_handle, &inner_param);
                let gen_spec = match &self.target {
                    GraphTarget::Generator(_) => Some(prepare_generator_mirror(
                        def,
                        &node_handle,
                        &inner_param,
                        expose,
                        &inner_label,
                        inner_min,
                        inner_max,
                        inner_default,
                        inner_convert,
                    )),
                    GraphTarget::Effect(_) => None,
                };
                (prev_in_set, static_slot, gen_spec)
            },
        );

        let Some((prev_in_set, static_slot, gen_spec)) = graph_result else {
            // Target didn't resolve — nothing to undo.
            self.reverse = NodeExposeReverse::None;
            return;
        };

        // Per-target mirror. Effect mirror touches EffectInstance +
        // host layer (envelopes); generator mirror touches the layer's
        // GeneratorParamState only.
        let mirror = match &self.target {
            GraphTarget::Effect(effect_id) => {
                let effect = match project.find_effect_by_id_mut(effect_id) {
                    Some(fx) => fx,
                    None => {
                        // Effect was deleted between graph borrow and
                        // mirror borrow. Capture just the graph bit so
                        // undo restores it; no mirror to record.
                        self.reverse = NodeExposeReverse::Captured {
                            prev_in_set,
                            mirror: MirrorReverse::Effect(EffectMirrorReverse::NoOp),
                        };
                        return;
                    }
                };
                let mut effect_mirror = mirror_effect_side(
                    effect,
                    &node_handle,
                    &inner_param,
                    expose,
                    static_slot,
                    &inner_label,
                    inner_min,
                    inner_max,
                    inner_default,
                    inner_convert,
                    inner_is_angle,
                );

                // Envelope orphan cleanup. Only matters when the
                // mirror is `RemovedUserBinding` — envelopes live on
                // the host Layer (not on `EffectInstance`, not on
                // `Master`), so capture needs a separate layer borrow.
                if let EffectMirrorReverse::RemovedUserBinding {
                    binding,
                    removed_envelope_state,
                    ..
                } = &mut effect_mirror
                {
                    let removed_id = binding.id.clone();
                    let host: Option<(manifold_core::LayerId, manifold_core::EffectTypeId)> =
                        project.timeline.layers.iter().find_map(|l| {
                            l.effects.as_ref().and_then(|fxs| {
                                fxs.iter()
                                    .find(|fx| &fx.id == effect_id)
                                    .map(|fx| (l.layer_id.clone(), fx.effect_type().clone()))
                            })
                        });
                    if let Some((layer_id, effect_type)) = host
                        && let Some((_, layer)) =
                            project.timeline.find_layer_by_id_mut(&layer_id)
                    {
                        let envs = layer.envelopes_mut();
                        let mut taken = Vec::new();
                        envs.retain(|e| {
                            let keep = !(e.target_effect_type == effect_type
                                && e.param_id == removed_id);
                            if !keep {
                                taken.push(e.clone());
                            }
                            keep
                        });
                        if !taken.is_empty() {
                            *removed_envelope_state = Some(RemovedEnvelopeState {
                                layer_id,
                                effect_type,
                                envelopes: taken,
                            });
                        }
                    }
                }
                MirrorReverse::Effect(effect_mirror)
            }
            GraphTarget::Generator(layer_id) => {
                // Generator side: apply gen_spec to the layer's
                // GeneratorParamState. The spec is pre-computed
                // against the graph borrow so we know exactly which
                // (binding, spec) pair was just appended or which
                // slot needs removal.
                let Some(spec) = gen_spec else {
                    // No spec means the graph borrow returned `None`
                    // for `prepare_generator_mirror` — defensive
                    // fall-through. Capture as NoOp.
                    self.reverse = NodeExposeReverse::Captured {
                        prev_in_set,
                        mirror: MirrorReverse::Generator(GeneratorMirrorReverse::NoOp),
                    };
                    return;
                };
                let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id) else {
                    // Layer vanished between graph borrow and gen-params
                    // borrow. Capture the graph bit only.
                    self.reverse = NodeExposeReverse::Captured {
                        prev_in_set,
                        mirror: MirrorReverse::Generator(GeneratorMirrorReverse::NoOp),
                    };
                    return;
                };
                let gen_mirror = apply_generator_mirror(layer, spec);
                MirrorReverse::Generator(gen_mirror)
            }
        };

        self.reverse = NodeExposeReverse::Captured {
            prev_in_set,
            mirror,
        };
    }

    fn undo(&mut self, project: &mut Project) {
        let reverse = std::mem::take(&mut self.reverse);
        let NodeExposeReverse::Captured {
            prev_in_set,
            mirror,
        } = reverse
        else {
            return;
        };

        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();

        match mirror {
            MirrorReverse::Effect(effect_mirror) => {
                // Envelope restore runs FIRST so its borrow of
                // `project.timeline` is released before the
                // effect-side restore needs its own walk through the
                // same data. Take the envelope payload off the mirror
                // up front; the effect-side restore doesn't use it.
                let envelope_state =
                    if let EffectMirrorReverse::RemovedUserBinding {
                        ref removed_envelope_state,
                        ..
                    } = effect_mirror
                    {
                        removed_envelope_state.as_ref().map(|s| {
                            (
                                s.layer_id.clone(),
                                s.effect_type.clone(),
                                s.envelopes.clone(),
                            )
                        })
                    } else {
                        None
                    };
                if let Some((layer_id, _effect_type, envelopes)) = envelope_state
                    && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&layer_id)
                {
                    layer.envelopes_mut().extend(envelopes);
                }

                if let GraphTarget::Effect(effect_id) = &self.target
                    && let Some(effect) = project.find_effect_by_id_mut(effect_id)
                {
                    unmirror_effect_side(effect, effect_mirror);
                }

                // Effect-side graph restore — bit only. The
                // generator-side restore handles its own graph
                // bookkeeping inline because the layer borrow
                // covers it.
                let _ = with_existing_target_graph_mut(project, &self.target, |def| {
                    restore_graph_exposed(def, &node_handle, &inner_param, prev_in_set);
                });
            }
            MirrorReverse::Generator(gen_mirror) => {
                if let GraphTarget::Generator(layer_id) = &self.target
                    && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
                {
                    unmirror_generator_side(
                        layer,
                        gen_mirror,
                        prev_in_set,
                        &node_handle,
                        &inner_param,
                    );
                }
            }
        }
    }

    fn description(&self) -> &str {
        if self.expose {
            "Expose Param"
        } else {
            "Hide Param"
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mirror_effect_side(
    effect: &mut manifold_core::effects::EffectInstance,
    node_handle: &str,
    inner_param: &str,
    expose: bool,
    static_slot: Option<usize>,
    inner_label: &str,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    inner_convert: manifold_core::effects::ParamConvert,
    inner_is_angle: bool,
) -> EffectMirrorReverse {
    use manifold_core::effects::{ParamSlot, UserParamBinding};

    if let Some(slot) = static_slot {
        // Static-block path: flip param_values[slot].exposed.
        let Some(s) = effect.param_values.get_mut(slot) else {
            return EffectMirrorReverse::NoOp;
        };
        if s.exposed == expose {
            return EffectMirrorReverse::NoOp;
        }
        let prev_exposed = s.exposed;
        s.exposed = expose;
        return EffectMirrorReverse::StaticSlot { slot, prev_exposed };
    }

    // Non-static path: append / remove a UserParamBinding.
    let existing_position = effect
        .user_param_bindings
        .iter()
        .position(|b| b.node_handle == node_handle && b.inner_param == inner_param);

    if expose {
        if existing_position.is_some() {
            return EffectMirrorReverse::NoOp;
        }
        let id = crate::commands::effects::generate_user_param_id(
            node_handle,
            inner_param,
            &effect.user_param_bindings,
        );
        let binding = UserParamBinding {
            id: id.clone(),
            label: inner_label.to_string(),
            node_handle: node_handle.to_string(),
            inner_param: inner_param.to_string(),
            min: inner_min,
            max: inner_max,
            default_value: inner_default,
            convert: inner_convert,
            is_angle: inner_is_angle,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        };
        effect.append_user_binding(binding);
        EffectMirrorReverse::AppendedUserBinding {
            user_param_id: id,
        }
    } else {
        let Some(position) = existing_position else {
            return EffectMirrorReverse::NoOp;
        };
        let binding = effect.user_param_bindings[position].clone();
        let binding_id = binding.id.clone();
        // Capture the slot value BEFORE removal (the slot lives at
        // static_count + position).
        let static_count = manifold_core::effect_definition_registry::try_get(effect.effect_type())
            .map(|def| def.param_count)
            .unwrap_or(0);
        let slot_idx = static_count + position;
        let slot_value = effect
            .param_values
            .get(slot_idx)
            .copied()
            .unwrap_or(ParamSlot::exposed(inner_default));
        // Prune any effect-local automation that referenced this
        // binding's id. After removal the id stops resolving anywhere
        // and the rows would silently never apply — capture them on
        // the reverse state so undo restores both the binding AND the
        // automation it carried.
        let removed_drivers = if let Some(ds) = effect.drivers.as_mut() {
            let mut taken = Vec::new();
            ds.retain(|d| {
                let keep = d.param_id != binding_id;
                if !keep {
                    taken.push(d.clone());
                }
                keep
            });
            if ds.is_empty() {
                effect.drivers = None;
            }
            taken
        } else {
            Vec::new()
        };
        let removed_ableton_mappings = if let Some(ms) = effect.ableton_mappings.as_mut() {
            let mut taken = Vec::new();
            ms.retain(|m| {
                let keep = m.param_id != binding_id;
                if !keep {
                    taken.push(m.clone());
                }
                keep
            });
            if ms.is_empty() {
                effect.ableton_mappings = None;
            }
            taken
        } else {
            Vec::new()
        };
        let _ = effect.remove_user_binding_by_id(&binding_id);
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            slot_value,
            removed_drivers,
            removed_ableton_mappings,
            // Envelope cleanup needs the layer borrow, not the effect
            // borrow — populated by the caller in `execute()` after
            // `mirror_effect_side` returns.
            removed_envelope_state: None,
        }
    }
}

fn unmirror_effect_side(
    effect: &mut manifold_core::effects::EffectInstance,
    mirror: EffectMirrorReverse,
) {
    match mirror {
        EffectMirrorReverse::NoOp => {}
        EffectMirrorReverse::StaticSlot { slot, prev_exposed } => {
            if let Some(s) = effect.param_values.get_mut(slot) {
                s.exposed = prev_exposed;
            }
        }
        EffectMirrorReverse::AppendedUserBinding { user_param_id } => {
            let _ = effect.remove_user_binding_by_id(&user_param_id);
        }
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            slot_value,
            removed_drivers,
            removed_ableton_mappings,
            // Envelope restore is handled separately in the command's
            // `undo()` because it needs the layer borrow, not the
            // effect borrow.
            removed_envelope_state: _,
        } => {
            let pos = position.min(effect.user_param_bindings.len());
            let binding_id = binding.id.clone();
            effect.user_param_bindings.insert(pos, binding);
            effect.user_param_bindings_version =
                effect.user_param_bindings_version.wrapping_add(1);
            if let Some(value_idx) = effect.param_id_to_value_index(&binding_id) {
                if value_idx <= effect.param_values.len() {
                    effect.param_values.insert(value_idx, slot_value);
                } else {
                    effect.param_values.push(slot_value);
                }
            }
            // Restore the automation rows that referenced this binding.
            // The same id now resolves through `param_id_to_value_index`
            // since we re-inserted the binding above.
            if !removed_drivers.is_empty() {
                effect
                    .drivers
                    .get_or_insert_with(Vec::new)
                    .extend(removed_drivers);
            }
            if !removed_ableton_mappings.is_empty() {
                effect
                    .ableton_mappings
                    .get_or_insert_with(Vec::new)
                    .extend(removed_ableton_mappings);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Generator-side mirror helpers
// ---------------------------------------------------------------------------

/// Specification of what the generator-side mirror needs to do to
/// `Layer.gen_params`, pre-computed against the graph borrow so the
/// downstream layer borrow has zero metadata work to do.
///
/// `RemovedUserBinding` captures a full `BindingDef` + `ParamSpecDef`
/// for undo, which makes it ~250 bytes — well past the
/// `large_enum_variant` lint threshold. The spec lives on the stack
/// for one execute() call and is then folded into
/// `GeneratorMirrorReverse` (which has the same shape constraint),
/// so the size is bounded by the undo-stack cap (200 entries). Boxing
/// just adds heap traffic for no benefit here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum GeneratorMirrorSpec {
    /// No metadata change needed — the binding already existed before
    /// (bundled or previously user-added). Caller still bumps the
    /// graph version through `with_target_graph_mut`.
    NoOp,
    /// A user-added binding was just appended to the graph's
    /// `preset_metadata.{params, bindings}`. The layer's
    /// `gp.param_values` needs a new tail slot to match.
    AppendedUserBinding {
        user_param_id: String,
        binding_index: usize,
        spec_index: usize,
        slot_index: usize,
        default_value: f32,
    },
    /// A user-added binding was just removed from the graph's
    /// `preset_metadata.{params, bindings}`. The layer's
    /// `gp.param_values` slot at `slot_index` needs removing, plus
    /// orphaned drivers/envelopes/Ableton mappings get pruned + captured.
    RemovedUserBinding {
        binding: manifold_core::effect_graph_def::BindingDef,
        spec: manifold_core::effect_graph_def::ParamSpecDef,
        binding_index: usize,
        spec_index: usize,
        slot_index: usize,
    },
}

/// Run inside `with_target_graph_mut` for a `GraphTarget::Generator`.
/// Either appends a new user-added `(BindingDef, ParamSpecDef)` pair
/// (when exposing a previously-unexposed inner param with no existing
/// binding), removes an existing user-added pair (when unexposing a
/// `user_added=true` binding), or returns `NoOp` (bundled bindings on
/// expose/unexpose just flip `exposed_params`).
#[allow(clippy::too_many_arguments)]
fn prepare_generator_mirror(
    def: &mut EffectGraphDef,
    node_handle: &str,
    inner_param: &str,
    expose: bool,
    inner_label: &str,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    inner_convert: manifold_core::effects::ParamConvert,
) -> GeneratorMirrorSpec {
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
    };

    // Resolve / create the preset metadata block. Generators
    // delivered as JSON presets always carry metadata; defensive
    // synth keeps the bookkeeping uniform if the graph is missing it.
    let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
        id: manifold_core::EffectTypeId::new(""),
        display_name: String::new(),
        category: String::new(),
        osc_prefix: String::new(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        params: Vec::new(),
        bindings: Vec::new(),
        skip_mode: Default::default(),
        param_aliases: Vec::new(),
        node_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
    });

    // Locate an existing binding for (handle, param). A bundled
    // binding stays in metadata across uncheck; a user-added binding
    // gets pulled along with its slot.
    let existing = meta
        .bindings
        .iter()
        .position(|b| match &b.target {
            BindingTarget::HandleNode { handle, param } => {
                handle == node_handle && param == inner_param
            }
            BindingTarget::Composite { .. } => false,
        });

    if expose {
        if existing.is_some() {
            // Bundled or already-added binding — `exposed_params`
            // bit is the only state change. Slot already exists.
            return GeneratorMirrorSpec::NoOp;
        }
        // Mint a stable user-binding id. Reuses the same generator
        // as effect-side user bindings for symmetry — drivers /
        // envelopes / Ableton bindings address by id and the
        // namespace is shared.
        let existing_ids: Vec<&str> =
            meta.bindings.iter().map(|b| b.id.as_str()).collect();
        let user_param_id = generate_unique_user_param_id_in(
            node_handle,
            inner_param,
            &existing_ids,
        );
        let spec = ParamSpecDef {
            id: user_param_id.clone(),
            name: inner_label.to_string(),
            min: inner_min,
            max: inner_max,
            default_value: inner_default,
            whole_numbers: matches!(
                inner_convert,
                manifold_core::effects::ParamConvert::IntRound
                    | manifold_core::effects::ParamConvert::EnumRound
                    | manifold_core::effects::ParamConvert::Trigger
            ),
            is_toggle: matches!(
                inner_convert,
                manifold_core::effects::ParamConvert::BoolThreshold
            ),
            is_trigger: matches!(
                inner_convert,
                manifold_core::effects::ParamConvert::Trigger
            ),
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
        };
        let binding = BindingDef {
            id: user_param_id.clone(),
            label: inner_label.to_string(),
            default_value: inner_default,
            target: BindingTarget::HandleNode {
                handle: node_handle.to_string(),
                param: inner_param.to_string(),
            },
            convert: inner_convert,
            user_added: true,
        };
        let spec_index = meta.params.len();
        let binding_index = meta.bindings.len();
        meta.params.push(spec);
        meta.bindings.push(binding);
        // Slot index matches spec position — `gp.param_values` is
        // positional against `preset_metadata.params`.
        let slot_index = spec_index;
        GeneratorMirrorSpec::AppendedUserBinding {
            user_param_id,
            binding_index,
            spec_index,
            slot_index,
            default_value: inner_default,
        }
    } else {
        // Unexpose: only user-added bindings get pulled. Bundled
        // bindings (user_added=false) survive — uncheck flips
        // `exposed_params` only, the slot stays so drivers /
        // Ableton / OSC mappings keep addressing it.
        let Some(binding_index) = existing else {
            return GeneratorMirrorSpec::NoOp;
        };
        let binding = &meta.bindings[binding_index];
        if !binding.user_added {
            return GeneratorMirrorSpec::NoOp;
        }
        let binding_id = binding.id.clone();
        let spec_index = meta.params.iter().position(|p| p.id == binding_id);
        // The spec MUST exist alongside its binding; both are
        // appended atomically by the expose path. Defensive bail if
        // somehow misaligned.
        let Some(spec_index) = spec_index else {
            return GeneratorMirrorSpec::NoOp;
        };
        let binding = meta.bindings.remove(binding_index);
        let spec = meta.params.remove(spec_index);
        let slot_index = spec_index;
        GeneratorMirrorSpec::RemovedUserBinding {
            binding,
            spec,
            binding_index,
            spec_index,
            slot_index,
        }
    }
}

/// Local user-id minter for generator user bindings. Mirrors the
/// effect-side `generate_user_param_id` shape but operates on an
/// `&[&str]` of existing ids (the generator graph stores them as
/// `BindingDef.id`s rather than as a `Vec<UserParamBinding>`).
fn generate_unique_user_param_id_in(
    node_handle: &str,
    inner_param: &str,
    existing_ids: &[&str],
) -> String {
    // Trim long handles to keep the user-facing id short. Same
    // convention as `crate::commands::effects::generate_user_param_id`.
    let short_handle = node_handle
        .rsplit_once('.')
        .map(|(_, tail)| tail)
        .unwrap_or(node_handle);
    let base = format!("user.{short_handle}.{inner_param}");
    let mut n: u32 = 1;
    loop {
        let candidate = format!("{base}.{n}");
        if !existing_ids.iter().any(|id| *id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Apply the precomputed `GeneratorMirrorSpec` to a layer's
/// `GeneratorParamState`. Returns the matching
/// `GeneratorMirrorReverse` for undo capture.
fn apply_generator_mirror(
    layer: &mut manifold_core::layer::Layer,
    spec: GeneratorMirrorSpec,
) -> GeneratorMirrorReverse {
    match spec {
        GeneratorMirrorSpec::NoOp => GeneratorMirrorReverse::NoOp,
        GeneratorMirrorSpec::AppendedUserBinding {
            user_param_id,
            binding_index,
            spec_index,
            slot_index,
            default_value,
        } => {
            // Extend gp.param_values to match the new
            // preset_metadata.params length. Slot lives at the tail.
            if let Some(gp) = layer.gen_params_mut() {
                if gp.param_values.len() <= slot_index {
                    while gp.param_values.len() < slot_index {
                        gp.param_values
                            .push(manifold_core::effects::ParamSlot::default());
                    }
                    gp.param_values
                        .push(manifold_core::effects::ParamSlot::exposed(default_value));
                } else {
                    gp.param_values[slot_index] =
                        manifold_core::effects::ParamSlot::exposed(default_value);
                }
                if let Some(base) = gp.base_param_values.as_mut() {
                    if base.len() <= slot_index {
                        while base.len() < slot_index {
                            base.push(0.0);
                        }
                        base.push(default_value);
                    } else {
                        base[slot_index] = default_value;
                    }
                }
            }
            GeneratorMirrorReverse::AppendedUserBinding {
                user_param_id,
                binding_index,
                spec_index,
                slot_index,
            }
        }
        GeneratorMirrorSpec::RemovedUserBinding {
            binding,
            spec,
            binding_index,
            spec_index,
            slot_index,
        } => {
            let binding_id = binding.id.clone();
            let mut slot_value = binding.default_value;
            let mut removed_drivers: Vec<manifold_core::effects::ParameterDriver> =
                Vec::new();
            let mut removed_envelopes: Vec<manifold_core::effects::ParamEnvelope> =
                Vec::new();
            let mut removed_ableton_mappings: Vec<
                manifold_core::ableton_mapping::AbletonParamMapping,
            > = Vec::new();
            if let Some(gp) = layer.gen_params_mut() {
                if slot_index < gp.param_values.len() {
                    slot_value = gp.param_values.remove(slot_index).value;
                }
                if let Some(base) = gp.base_param_values.as_mut()
                    && slot_index < base.len()
                {
                    base.remove(slot_index);
                }
                // Prune drivers / envelopes / Ableton referencing the
                // removed binding's id. Each automation row references
                // params by `param_id` (a stable string), not by slot
                // index, so id-based retain is the correct semantics.
                if let Some(ds) = gp.drivers.as_mut() {
                    ds.retain(|d| {
                        let keep = d.param_id != binding_id;
                        if !keep {
                            removed_drivers.push(d.clone());
                        }
                        keep
                    });
                    if ds.is_empty() {
                        gp.drivers = None;
                    }
                }
                if let Some(es) = gp.envelopes.as_mut() {
                    es.retain(|e| {
                        let keep = e.param_id != binding_id;
                        if !keep {
                            removed_envelopes.push(e.clone());
                        }
                        keep
                    });
                    if es.is_empty() {
                        gp.envelopes = None;
                    }
                }
                if let Some(ms) = gp.ableton_mappings.as_mut() {
                    ms.retain(|m| {
                        let keep = m.param_id != binding_id;
                        if !keep {
                            removed_ableton_mappings.push(m.clone());
                        }
                        keep
                    });
                    if ms.is_empty() {
                        gp.ableton_mappings = None;
                    }
                }
            }
            GeneratorMirrorReverse::RemovedUserBinding {
                binding,
                spec,
                binding_index,
                spec_index,
                slot_index,
                slot_value,
                removed_drivers,
                removed_envelopes,
                removed_ableton_mappings,
            }
        }
    }
}

/// Reverse `apply_generator_mirror` for undo. Restores both the
/// `gp.param_values` slot + automation AND the graph-side
/// `preset_metadata.{params, bindings}` entries — one layer borrow
/// covers everything because the graph lives inside `Layer`.
///
/// `prev_in_set` + `node_handle` + `inner_param` come from the
/// outer `NodeExposeReverse::Captured` state so we can restore the
/// `exposed_params` bit in the same pass, saving the
/// `with_existing_target_graph_mut` call that the effect-side undo
/// uses at the end.
fn unmirror_generator_side(
    layer: &mut manifold_core::layer::Layer,
    mirror: GeneratorMirrorReverse,
    prev_in_set: bool,
    node_handle: &str,
    inner_param: &str,
) {
    // Restore the graph-side metadata first so the slot it points
    // at exists / doesn't exist before we touch `gp.param_values`.
    if let Some(def) = layer.generator_graph.as_mut() {
        match &mirror {
            GeneratorMirrorReverse::AppendedUserBinding {
                binding_index,
                spec_index,
                ..
            } => {
                if let Some(meta) = def.preset_metadata.as_mut() {
                    if *binding_index < meta.bindings.len() {
                        meta.bindings.remove(*binding_index);
                    }
                    if *spec_index < meta.params.len() {
                        meta.params.remove(*spec_index);
                    }
                }
            }
            GeneratorMirrorReverse::RemovedUserBinding {
                binding,
                spec,
                binding_index,
                spec_index,
                ..
            } => {
                if let Some(meta) = def.preset_metadata.as_mut() {
                    let bi = (*binding_index).min(meta.bindings.len());
                    let si = (*spec_index).min(meta.params.len());
                    meta.bindings.insert(bi, binding.clone());
                    meta.params.insert(si, spec.clone());
                }
            }
            GeneratorMirrorReverse::NoOp => {}
        }
        // Exposure bit lives on the node's `exposed_params` set —
        // restore it inline since we already hold the graph borrow.
        restore_graph_exposed(def, node_handle, inner_param, prev_in_set);
        // Bump version so the renderer rebuilds against the
        // restored shape. `with_existing_target_graph_mut` does this
        // for the effect-side; we mirror it manually here.
        layer.generator_graph_version = layer.generator_graph_version.wrapping_add(1);
    }

    match mirror {
        GeneratorMirrorReverse::NoOp => {}
        GeneratorMirrorReverse::AppendedUserBinding { slot_index, .. } => {
            if let Some(gp) = layer.gen_params_mut() {
                if slot_index < gp.param_values.len() {
                    gp.param_values.remove(slot_index);
                }
                if let Some(base) = gp.base_param_values.as_mut()
                    && slot_index < base.len()
                {
                    base.remove(slot_index);
                }
            }
        }
        GeneratorMirrorReverse::RemovedUserBinding {
            slot_index,
            slot_value,
            removed_drivers,
            removed_envelopes,
            removed_ableton_mappings,
            ..
        } => {
            if let Some(gp) = layer.gen_params_mut() {
                let restored = manifold_core::effects::ParamSlot::exposed(slot_value);
                if slot_index <= gp.param_values.len() {
                    gp.param_values.insert(slot_index, restored);
                } else {
                    gp.param_values.push(restored);
                }
                if let Some(base) = gp.base_param_values.as_mut() {
                    if slot_index <= base.len() {
                        base.insert(slot_index, slot_value);
                    } else {
                        base.push(slot_value);
                    }
                }
                if !removed_drivers.is_empty() {
                    gp.drivers
                        .get_or_insert_with(Vec::new)
                        .extend(removed_drivers);
                }
                if !removed_envelopes.is_empty() {
                    gp.envelopes
                        .get_or_insert_with(Vec::new)
                        .extend(removed_envelopes);
                }
                if !removed_ableton_mappings.is_empty() {
                    gp.ableton_mappings
                        .get_or_insert_with(Vec::new)
                        .extend(removed_ableton_mappings);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::EffectId;
    use manifold_core::EffectTypeId;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
    use manifold_core::effects::EffectInstance;

    /// Catalog default for a Mirror-like graph: source → uv_transform
    /// → mix → final_output, four nodes plus four wires. Mirrors the
    /// shape the runtime `build_mirror` produces.
    fn mirror_catalog_default() -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 1,
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 2,
                    type_id: "node.mix".to_string(),
                    handle: Some("mix".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 3,
                    type_id: "system.final_output".to_string(),
                    handle: Some("final_output".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                },
            ],
            wires: vec![
                EffectGraphWire {
                    from_node: 0,
                    from_port: "out".to_string(),
                    to_node: 1,
                    to_port: "source".to_string(),
                },
                EffectGraphWire {
                    from_node: 0,
                    from_port: "out".to_string(),
                    to_node: 2,
                    to_port: "a".to_string(),
                },
                EffectGraphWire {
                    from_node: 1,
                    from_port: "out".to_string(),
                    to_node: 2,
                    to_port: "b".to_string(),
                },
                EffectGraphWire {
                    from_node: 2,
                    from_port: "out".to_string(),
                    to_node: 3,
                    to_port: "in".to_string(),
                },
            ],
        }
    }

    /// Project with one master Mirror effect, graph: None.
    fn project_with_one_master_effect() -> (Project, EffectId) {
        let mut project = Project::default();
        let fx = EffectInstance::new(EffectTypeId::new("Mirror"));
        let id = fx.id.clone();
        project.settings.master_effects.push(fx);
        (project, id)
    }

    #[test]
    fn add_graph_node_lifts_from_none_and_appends_node() {
        let (mut project, id) = project_with_one_master_effect();
        let mut cmd = AddGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            "node.blur".to_string(),
            Some((50.0, 60.0)),
            mirror_catalog_default(),
        );

        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(fx.graph.is_some(), "lift should populate graph");
        let def = fx.graph.as_ref().unwrap();
        // Catalog default (4 nodes) + the new Blur = 5.
        assert_eq!(def.nodes.len(), 5);
        let new_id = cmd.new_node_id().expect("id minted");
        let new_node = def.nodes.iter().find(|n| n.id == new_id).unwrap();
        assert_eq!(new_node.type_id, "node.blur");
        assert_eq!(new_node.editor_pos, Some((50.0, 60.0)));
        assert_eq!(fx.graph_version, 1);
    }

    #[test]
    fn add_graph_node_undo_removes_node() {
        let (mut project, id) = project_with_one_master_effect();
        let mut cmd = AddGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            "node.blur".to_string(),
            None,
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);
        cmd.undo(&mut project);

        let fx = project.find_effect_by_id(&id).unwrap();
        // Graph is still Some after undo (no un-lift), but with
        // catalog-default contents.
        let def = fx.graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 4);
        assert_eq!(fx.graph_version, 2); // bumped twice (execute + undo)
    }

    #[test]
    fn remove_graph_node_also_removes_incident_wires() {
        let (mut project, id) = project_with_one_master_effect();
        // Pre-populate graph with the catalog default.
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        let mut cmd =
            RemoveGraphNodeCommand::new(GraphTarget::Effect(id.clone()), 1, mirror_catalog_default());
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 3);
        // Wires touching node 1 (src→uv, uv→mix.b) are gone.
        assert!(def.wires.iter().all(|w| w.from_node != 1 && w.to_node != 1));
        // The src→mix.a wire is intact.
        assert!(def
            .wires
            .iter()
            .any(|w| w.from_node == 0 && w.to_port == "a"));
    }

    #[test]
    fn remove_graph_node_undo_restores_node_and_wires() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        let mut cmd =
            RemoveGraphNodeCommand::new(GraphTarget::Effect(id.clone()), 1, mirror_catalog_default());
        cmd.execute(&mut project);
        cmd.undo(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 4);
        assert_eq!(def.wires.len(), 4);
    }

    #[test]
    fn connect_ports_displaces_existing_wire_and_undo_restores() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        // Rewire mix.b from uv_transform → directly from source.
        let mut cmd = ConnectPortsCommand::new(
            GraphTarget::Effect(id.clone()),
            0,
            "out".to_string(),
            2,
            "b".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        // mix.b is now fed from node 0 (source), not node 1 (uv).
        let mix_b = def
            .wires
            .iter()
            .find(|w| w.to_node == 2 && w.to_port == "b")
            .unwrap();
        assert_eq!(mix_b.from_node, 0);

        cmd.undo(&mut project);
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let mix_b = def
            .wires
            .iter()
            .find(|w| w.to_node == 2 && w.to_port == "b")
            .unwrap();
        assert_eq!(mix_b.from_node, 1, "undo restores original uv→mix.b wire");
    }

    #[test]
    fn disconnect_ports_removes_wire_and_undo_restores() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        let mut cmd = DisconnectPortsCommand::new(
            GraphTarget::Effect(id.clone()),
            2,
            "a".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        assert!(def
            .wires
            .iter()
            .all(|w| !(w.to_node == 2 && w.to_port == "a")));

        cmd.undo(&mut project);
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        assert!(def
            .wires
            .iter()
            .any(|w| w.to_node == 2 && w.to_port == "a"));
    }

    #[test]
    fn move_graph_node_updates_editor_pos_and_undo_restores() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        let mut cmd =
            MoveGraphNodeCommand::new(GraphTarget::Effect(id.clone()), 1, (100.0, 200.0), mirror_catalog_default());
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(node.editor_pos, Some((100.0, 200.0)));

        cmd.undo(&mut project);
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(node.editor_pos, None);
    }

    #[test]
    fn set_graph_node_param_inserts_and_undo_restores_absence() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph =
            Some(mirror_catalog_default());

        let mut cmd = SetGraphNodeParamCommand::new(
            GraphTarget::Effect(id.clone()),
            1,
            "mode".to_string(),
            SerializedParamValue::Enum { value: 7 },
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(
            node.params.get("mode"),
            Some(&SerializedParamValue::Enum { value: 7 })
        );

        cmd.undo(&mut project);
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert!(!node.params.contains_key("mode"), "undo removes the key");
    }

    #[test]
    fn set_graph_node_param_undo_restores_previous_value() {
        let (mut project, id) = project_with_one_master_effect();
        let mut def = mirror_catalog_default();
        // Pre-seed node 1 with mode=3 so undo has something to restore.
        def.nodes
            .iter_mut()
            .find(|n| n.id == 1)
            .unwrap()
            .params
            .insert("mode".to_string(), SerializedParamValue::Enum { value: 3 });
        project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

        let mut cmd = SetGraphNodeParamCommand::new(
            GraphTarget::Effect(id.clone()),
            1,
            "mode".to_string(),
            SerializedParamValue::Enum { value: 7 },
            def,
        );
        cmd.execute(&mut project);
        cmd.undo(&mut project);

        let after = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(
            node.params.get("mode"),
            Some(&SerializedParamValue::Enum { value: 3 }),
        );
    }

    #[test]
    fn revert_clears_graph_and_undo_restores_it() {
        let (mut project, id) = project_with_one_master_effect();
        // Diverge by adding a Blur — graph now Some(...).
        let mut add = AddGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            "node.blur".to_string(),
            None,
            mirror_catalog_default(),
        );
        add.execute(&mut project);
        assert!(project.find_effect_by_id(&id).unwrap().graph.is_some());

        let mut revert = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
        revert.execute(&mut project);
        assert!(
            project.find_effect_by_id(&id).unwrap().graph.is_none(),
            "revert clears the per-card override"
        );

        revert.undo(&mut project);
        assert!(
            project.find_effect_by_id(&id).unwrap().graph.is_some(),
            "undo restores the per-card override"
        );
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        assert!(def.nodes.iter().any(|n| n.type_id == "node.blur"));
    }

    #[test]
    fn revert_on_already_default_is_a_no_op() {
        // graph: None to start. Revert should be silent (no panic, no
        // change), and undo should also be silent.
        let (mut project, id) = project_with_one_master_effect();
        assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());

        let mut revert = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
        revert.execute(&mut project);
        assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());

        revert.undo(&mut project);
        assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());
    }

    /// End-to-end: lift via AddGraphNode, save to JSON, reload, verify
    /// the per-card graph survived. Phase 3's load-bearing test for
    /// "persistent edits across restart".
    #[test]
    fn graph_edits_survive_json_round_trip() {
        let (mut project, id) = project_with_one_master_effect();
        let mut cmd = AddGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            "node.blur".to_string(),
            Some((10.0, 20.0)),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        // Serialize just the EffectInstance — what the project save
        // path emits per effect.
        let fx = project.find_effect_by_id(&id).unwrap();
        let json = serde_json::to_string(fx).unwrap();
        let back: manifold_core::effects::EffectInstance =
            serde_json::from_str(&json).unwrap();

        assert!(back.graph.is_some(), "graph field survived round-trip");
        let def = back.graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 5, "appended Blur survived");
        assert!(def.nodes.iter().any(|n| n.type_id == "node.blur"));
    }

    // ─── Generator-target parity ────────────────────────────────────
    //
    // The same commands targeting `GraphTarget::Generator(layer_id)`
    // must mutate `Layer::generator_graph` rather than `EffectInstance::graph`.
    // These tests exercise the unified pipeline against the generator
    // persistence path — proves there's truly one set of commands.

    use manifold_core::LayerId;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;

    /// Project with one timeline layer, no generator override.
    fn project_with_one_generator_layer() -> (Project, LayerId) {
        let mut project = Project::default();
        let layer = Layer::new("Test Layer".to_string(), LayerType::Generator, 0);
        let lid = layer.layer_id.clone();
        project.timeline.layers.push(layer);
        (project, lid)
    }

    #[test]
    fn add_graph_node_against_generator_target_lifts_layer_generator_graph() {
        let (mut project, lid) = project_with_one_generator_layer();
        let mut cmd = AddGraphNodeCommand::new(
            GraphTarget::Generator(lid.clone()),
            "node.uv_field".to_string(),
            Some((40.0, 50.0)),
            mirror_catalog_default(),
        );

        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        assert!(
            layer.generator_graph.is_some(),
            "generator_graph must lift from None on first edit",
        );
        let def = layer.generator_graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 5, "catalog 4 + new node = 5");
        assert!(def.nodes.iter().any(|n| n.type_id == "node.uv_field"));
        assert_eq!(layer.generator_graph_version, 1);
    }

    #[test]
    fn revert_clears_generator_graph_and_undo_restores_it() {
        let (mut project, lid) = project_with_one_generator_layer();
        // Pre-populate with the catalog default (acts as an existing
        // user-edited override).
        project
            .timeline
            .find_layer_by_id_mut(&lid)
            .unwrap()
            .1
            .generator_graph = Some(mirror_catalog_default());

        let mut revert = RevertEffectGraphCommand::new(GraphTarget::Generator(lid.clone()));
        revert.execute(&mut project);
        assert!(
            project
                .timeline
                .find_layer_by_id(&lid)
                .unwrap()
                .1
                .generator_graph
                .is_none(),
            "execute clears the override",
        );

        revert.undo(&mut project);
        assert!(
            project
                .timeline
                .find_layer_by_id(&lid)
                .unwrap()
                .1
                .generator_graph
                .is_some(),
            "undo restores the previous override",
        );
    }

    // ─── Toggle Node Param Expose (unified) ─────────────────────────
    //
    // The same command lights up both Effect-hosted and Generator-
    // hosted graphs. These tests pin the contract for each direction.

    #[test]
    fn toggle_node_param_expose_against_generator_flips_graph_exposed_set() {
        let (mut project, lid) = project_with_one_generator_layer();
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
        );

        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(
            node.exposed_params.contains("rotation"),
            "expose flips the graph exposed_params set"
        );

        // Undo flips it back.
        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(
            !node.exposed_params.contains("rotation"),
            "undo restores prior exposed_params state"
        );
    }

    #[test]
    fn exposing_a_non_preset_param_on_generator_appends_user_binding_and_grows_param_values() {
        // Regression: clicking the expose checkbox on a generator's
        // inner-node param that has NO preset binding (e.g.
        // `node.render_lines:animate` on the Wireframe preset) must
        // synthesize a user-added BindingDef + ParamSpecDef in the
        // graph's preset_metadata AND extend gp.param_values by one
        // slot so the outer card has somewhere to render it.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, ParamSpecDef,
            PresetMetadata,
        };
        use manifold_core::effects::ParamConvert;
        use manifold_core::generator_type_id::GeneratorTypeId;

        // Wireframe-like preset: two bundled bindings ("shape" → render.shape,
        // "scale" → render.scale) plus an inner node `render` whose
        // `animate` param is NOT bound. param_values has two bundled
        // slots.
        let preset_def = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: EffectTypeId::new("test.wireframe"),
                display_name: "Wireframe".into(),
                category: "Procedural".into(),
                osc_prefix: "wireframe".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![
                    ParamSpecDef {
                        id: "shape".into(),
                        name: "Shape".into(),
                        min: 0.0,
                        max: 4.0,
                        default_value: 0.0,
                        whole_numbers: true,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                    },
                    ParamSpecDef {
                        id: "scale".into(),
                        name: "Scale".into(),
                        min: 0.25,
                        max: 3.0,
                        default_value: 1.0,
                        whole_numbers: false,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::HandleNode {
                            handle: "render".into(),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                    },
                    BindingDef {
                        id: "scale".into(),
                        label: "Scale".into(),
                        default_value: 1.0,
                        target: BindingTarget::HandleNode {
                            handle: "render".into(),
                            param: "scale".into(),
                        },
                        convert: ParamConvert::Float,
                        user_added: false,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                node_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.render_lines".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.generator_graph = Some(preset_def());
            // gen_params starts with the two bundled slot values.
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(GeneratorTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            // Override values after init — the registry doesn't know
            // about our synthetic preset, so init may leave the vec
            // empty. Force the bundled slot count to match the preset.
            gp.param_values = vec![
                manifold_core::effects::ParamSlot::exposed(0.0),
                manifold_core::effects::ParamSlot::exposed(1.0),
            ];
            gp.base_param_values = Some(vec![0.0, 1.0]);
        }

        // Expose `render.animate` — has no preset binding, so the
        // command must synthesize a user-added entry.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            "render".to_string(),
            "animate".to_string(),
            true,
            preset_def(),
            "Animate".to_string(),
            0.0,
            1.0,
            0.0,
            ParamConvert::BoolThreshold,
            false,
        );
        expose.execute(&mut project);

        // Assert: preset_metadata grew by one entry in both lists,
        // marked user_added=true.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(
            meta.params.len(),
            3,
            "preset_metadata.params grew by one user-added entry"
        );
        assert_eq!(
            meta.bindings.len(),
            3,
            "preset_metadata.bindings grew by one user-added entry"
        );
        let new_binding = meta.bindings.last().unwrap();
        assert!(
            new_binding.user_added,
            "newly appended binding is flagged user_added=true"
        );
        assert!(
            matches!(
                &new_binding.target,
                BindingTarget::HandleNode { handle, param }
                    if handle == "render" && param == "animate"
            ),
            "new binding routes to render.animate"
        );

        // The id should be auto-generated; capture for later
        // assertions on undo.
        let user_param_id = new_binding.id.clone();
        assert!(
            user_param_id.starts_with("user.render.animate."),
            "id follows the user.<handle>.<param>.<n> convention, got `{user_param_id}`"
        );

        // gp.param_values grew by one to match.
        let gp = layer.gen_params().unwrap();
        assert_eq!(
            gp.param_values.len(),
            3,
            "param_values grew by one slot for the user-added binding"
        );

        // exposed_params on the render node now contains "animate".
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("render"))
            .unwrap();
        assert!(
            render_node.exposed_params.contains("animate"),
            "render.animate is now in exposed_params"
        );

        // Undo restores everything.
        expose.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo removes the user-added param");
        assert_eq!(
            meta.bindings.len(),
            2,
            "undo removes the user-added binding"
        );
        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.param_values.len(), 2, "undo pops the user-added slot");
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("render"))
            .unwrap();
        assert!(
            !render_node.exposed_params.contains("animate"),
            "undo restores exposed_params"
        );

        // Re-execute → state matches post-execute.
        expose.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.bindings.len(), 3);
        assert_eq!(meta.bindings.last().unwrap().id, user_param_id);

        // user_added flag survives JSON round-trip.
        let json = serde_json::to_string(def).unwrap();
        let reloaded: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let reloaded_meta = reloaded.preset_metadata.as_ref().unwrap();
        assert_eq!(reloaded_meta.bindings.len(), 3);
        assert!(
            reloaded_meta.bindings.last().unwrap().user_added,
            "user_added=true survives serialization"
        );
        // Bundled bindings serialize without the field set; on
        // deserialize the default `false` should kick in.
        assert!(
            !reloaded_meta.bindings[0].user_added,
            "bundled binding stays user_added=false after round-trip"
        );
    }

    #[test]
    fn unexposing_a_user_added_generator_binding_removes_metadata_and_shrinks_param_values() {
        // The inverse of the test above: unexpose a previously
        // user-added binding. Removes the metadata + slot + any
        // referencing automation (drivers / envelopes / Ableton),
        // captures for undo.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, ParamSpecDef,
            PresetMetadata,
        };
        use manifold_core::effects::{ParamConvert, ParamEnvelope, ParameterDriver};
        use manifold_core::generator_type_id::GeneratorTypeId;
        use manifold_core::types::{BeatDivision, DriverWaveform};

        // Preset already carries a user-added binding (simulates
        // "user-added in a prior session, now loaded from a save
        // file"). One bundled binding + one user-added binding.
        let preset_def_with_user_added = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: EffectTypeId::new("test.wireframe"),
                display_name: "Wireframe".into(),
                category: "Procedural".into(),
                osc_prefix: "wireframe".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![
                    ParamSpecDef {
                        id: "shape".into(),
                        name: "Shape".into(),
                        min: 0.0,
                        max: 4.0,
                        default_value: 0.0,
                        whole_numbers: true,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                    },
                    ParamSpecDef {
                        id: "user.render.animate.1".into(),
                        name: "Animate".into(),
                        min: 0.0,
                        max: 1.0,
                        default_value: 0.0,
                        whole_numbers: false,
                        is_toggle: true,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::HandleNode {
                            handle: "render".into(),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                    },
                    BindingDef {
                        id: "user.render.animate.1".into(),
                        label: "Animate".into(),
                        default_value: 0.0,
                        target: BindingTarget::HandleNode {
                            handle: "render".into(),
                            param: "animate".into(),
                        },
                        convert: ParamConvert::BoolThreshold,
                        user_added: true,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                node_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.render_lines".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: {
                    let mut s = std::collections::BTreeSet::new();
                    s.insert("animate".to_string());
                    s
                },
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.generator_graph = Some(preset_def_with_user_added());
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(GeneratorTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            gp.param_values = vec![
                manifold_core::effects::ParamSlot::exposed(0.0),
                manifold_core::effects::ParamSlot::exposed(0.75),
            ]; // bundled `shape` + user-added `animate`
            gp.base_param_values = Some(vec![0.0, 0.75]);
            // Attach a driver + envelope on the user-added id — they
            // should get pruned on unexpose and restored on undo.
            gp.drivers = Some(vec![ParameterDriver {
                param_id: std::borrow::Cow::Owned("user.render.animate.1".to_string()),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.5,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            gp.envelopes = Some(vec![ParamEnvelope::new_for_gen(std::borrow::Cow::Owned(
                "user.render.animate.1".to_string(),
            ))]);
        }

        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            "render".to_string(),
            "animate".to_string(),
            false,
            preset_def_with_user_added(),
            "Animate".to_string(),
            0.0,
            1.0,
            0.0,
            ParamConvert::BoolThreshold,
            false,
        );
        unexpose.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 1, "user-added param removed");
        assert_eq!(meta.bindings.len(), 1, "user-added binding removed");
        assert_eq!(meta.bindings[0].id, "shape", "bundled binding survives");

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.param_values.len(), 1, "user-added slot removed");
        assert_eq!(gp.param_values[0].value, 0.0, "bundled `shape` value intact");
        assert!(
            gp.drivers.is_none() || gp.drivers.as_ref().unwrap().is_empty(),
            "driver referencing user-added id pruned"
        );
        assert!(
            gp.envelopes.is_none() || gp.envelopes.as_ref().unwrap().is_empty(),
            "envelope referencing user-added id pruned"
        );

        // Undo restores everything.
        unexpose.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo restores user-added param");
        assert_eq!(meta.bindings.len(), 2, "undo restores user-added binding");
        assert_eq!(meta.bindings[1].id, "user.render.animate.1");
        assert!(meta.bindings[1].user_added);

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.param_values.len(), 2, "undo restores the slot");
        assert!(
            (gp.param_values[1].value - 0.75).abs() < f32::EPSILON,
            "slot value (0.75) restored"
        );
        assert_eq!(
            gp.drivers.as_ref().map(|d| d.len()).unwrap_or(0),
            1,
            "driver restored"
        );
        assert_eq!(
            gp.envelopes.as_ref().map(|e| e.len()).unwrap_or(0),
            1,
            "envelope restored"
        );
    }

    #[test]
    fn unexposing_a_user_binding_prunes_and_restores_orphan_automation() {
        // When the user un-checks a non-preset-bound exposure on an
        // effect (i.e. it was previously exposed via a UserParamBinding),
        // any drivers / Ableton mappings that referenced the binding's
        // param_id would otherwise become orphans — still in the
        // project file, never matched at resolve time. The unified
        // command prunes them on unexpose and restores them on undo.
        use manifold_core::ableton_mapping::{
            AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus,
            AbletonParamMapping,
        };
        use manifold_core::effects::{ParamConvert, ParameterDriver};
        use manifold_core::types::{BeatDivision, DriverWaveform};

        // Set up an effect with one user-exposed inner param + driver
        // + Ableton mapping that target its synthesised id.
        let mut project = Project::default();
        let effect_id = EffectId::new("orphan-cleanup-test");
        let mut fx = EffectInstance::new(EffectTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        // Expose first.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
        );
        expose.execute(&mut project);

        // Now attach a driver + ableton mapping to the synthesised
        // user_param_id.
        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            assert_eq!(fx.user_param_bindings.len(), 1);
            fx.user_param_bindings[0].id.clone()
        };
        {
            let fx = project.find_effect_by_id_mut(&effect_id).unwrap();
            fx.drivers = Some(vec![ParameterDriver {
                param_id: std::borrow::Cow::Owned(user_param_id.clone()),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.5,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            fx.ableton_mappings = Some(vec![AbletonParamMapping {
                param_id: std::borrow::Cow::Owned(user_param_id.clone()),
                address: AbletonMacroAddress {
                    track_id: 0,
                    device_id: 0,
                    param_id: 0,
                    device_identity: AbletonDeviceIdentity {
                        device_class_name: "InstrumentGroupDevice".into(),
                    },
                    track_name: "Master".into(),
                    device_name: "Manifold".into(),
                    macro_name: "Macro 1".into(),
                },
                range_min: 0.0,
                range_max: 1.0,
                inverted: false,
                legacy_param_index: None,
                last_value: 0.0,
                status: AbletonMappingStatus::Active,
            }]);
        }

        // Unexpose. Drivers + Ableton mappings must be pruned.
        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
        );
        unexpose.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert!(
            fx.drivers.is_none() || fx.drivers.as_ref().unwrap().is_empty(),
            "drivers pruned on unexpose"
        );
        assert!(
            fx.ableton_mappings.is_none()
                || fx.ableton_mappings.as_ref().unwrap().is_empty(),
            "ableton_mappings pruned on unexpose"
        );

        // Undo restores both.
        unexpose.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.user_param_bindings.len(), 1, "binding restored");
        assert_eq!(
            fx.drivers.as_ref().map(|d| d.len()).unwrap_or(0),
            1,
            "driver restored"
        );
        assert_eq!(
            fx.ableton_mappings.as_ref().map(|m| m.len()).unwrap_or(0),
            1,
            "ableton mapping restored"
        );
        assert_eq!(
            fx.drivers.as_ref().unwrap()[0].param_id,
            std::borrow::Cow::<'static, str>::Owned(user_param_id.clone()),
        );
    }

    #[test]
    fn unexposing_a_user_binding_on_layer_effect_prunes_and_restores_envelopes() {
        // Same shape as the driver/Ableton orphan-cleanup test, but
        // for envelopes — which live on the layer, not the effect.
        // The unified command walks the timeline to find the host
        // layer, prunes envelopes matching the binding's (effect_type,
        // param_id), captures them, and restores on undo.
        use manifold_core::effects::{ParamConvert, ParamEnvelope};
        use manifold_core::layer::Layer;
        use manifold_core::types::LayerType;

        let effect_type = EffectTypeId::new("test.mirror");
        let effect_id = EffectId::new("envelope-cleanup-test");

        // Layer-hosted effect (not master — master has no envelopes).
        let mut project = Project::default();
        let mut layer = Layer::new("Test".to_string(), LayerType::Generator, 0);
        let layer_id = layer.layer_id.clone();
        let mut fx = EffectInstance::new(effect_type.clone());
        fx.id = effect_id.clone();
        layer.effects = Some(vec![fx]);
        project.timeline.layers.push(layer);

        // Expose first, attach an envelope to the synthesised id.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
        );
        expose.execute(&mut project);

        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            fx.user_param_bindings[0].id.clone()
        };
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            layer.envelopes_mut().push(ParamEnvelope::new_for_effect(
                effect_type.clone(),
                user_param_id.clone(),
            ));
            // Add an unrelated envelope that should NOT get pruned —
            // different param_id.
            layer.envelopes_mut().push(ParamEnvelope::new_for_effect(
                effect_type.clone(),
                "unrelated.param".to_string(),
            ));
        }

        // Unexpose. The matching envelope must be pruned; the unrelated
        // one must survive.
        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
        );
        unexpose.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let envs = layer.envelopes.as_deref().unwrap_or(&[]);
        assert_eq!(envs.len(), 1, "matching envelope pruned, unrelated kept");
        assert_eq!(envs[0].param_id, "unrelated.param");

        // Undo restores the pruned envelope alongside the binding.
        unexpose.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let envs = layer.envelopes.as_deref().unwrap_or(&[]);
        assert_eq!(envs.len(), 2, "matching envelope restored");
        assert!(
            envs.iter().any(|e| e.param_id == user_param_id),
            "restored envelope points back at the binding's id"
        );
    }

    #[test]
    fn unchecking_a_preset_bound_param_sticks_across_persistence() {
        // Regression: when the user UNCHECKS a preset-bound param,
        // the next snapshot must reflect the uncheck. Previously the
        // `into_graph` binding backfill ran unconditionally and
        // re-set the exposure, masking the user's intent.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
            EFFECT_GRAPH_VERSION_WITH_METADATA,
        };
        use manifold_core::effects::ParamConvert;

        // Build a tiny preset def: one node (`gen` with a `pattern`
        // param) with a single binding (outer "Pattern" → gen.pattern).
        let preset_def_with_pattern_binding = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("test-preset".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: EffectTypeId::new("test.plasma"),
                display_name: "Test".into(),
                category: "Procedural".into(),
                osc_prefix: "test".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: "pattern".into(),
                    name: "Pattern".into(),
                    min: 0.0,
                    max: 7.0,
                    default_value: 0.0,
                    whole_numbers: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: vec![],
                    format_string: None,
                    osc_suffix: String::new(),
                }],
                bindings: vec![BindingDef {
                    id: "pattern".into(),
                    label: "Pattern".into(),
                    default_value: 0.0,
                    target: BindingTarget::HandleNode {
                        handle: "gen".into(),
                        param: "pattern".into(),
                    },
                    convert: ParamConvert::EnumRound,
                    user_added: false,
                }],
                skip_mode: Default::default(),
                param_aliases: vec![],
                node_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.plasma_pattern_2d".to_string(),
                handle: Some("gen".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
            }],
            wires: vec![],
        };

        // Use a generator target so we don't drag in the effect-side
        // mirror. Same exposure semantics apply for both.
        let (mut project, lid) = project_with_one_generator_layer();

        // Pre-populate the layer's override with the preset def
        // (simulates "graph has been touched once already" — needed
        // because `with_target_graph_mut` would otherwise clone the
        // catalog_default, and we want a deterministic starting state).
        project
            .timeline
            .find_layer_by_id_mut(&lid)
            .unwrap()
            .1
            .generator_graph = Some(preset_def_with_pattern_binding());

        // UNCHECK Pattern.
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            "gen".to_string(),
            "pattern".to_string(),
            false,
            preset_def_with_pattern_binding(),
            "Pattern".to_string(),
            0.0,
            7.0,
            0.0,
            ParamConvert::EnumRound,
            false,
        );
        cmd.execute(&mut project);

        // The def must NOT contain "pattern" in exposed_params for
        // the "gen" node.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("gen"))
            .unwrap();
        assert!(
            !node.exposed_params.contains("pattern"),
            "uncheck removes pattern from exposed_params"
        );

        // Now persist + reload: serde JSON round-trip simulating a
        // save/reload cycle.
        let json = serde_json::to_string(def).unwrap();
        let reloaded: EffectGraphDef = serde_json::from_str(&json).unwrap();
        // The reloaded def must STILL not have pattern exposed. The
        // semantics: an empty exposed_params set on a node coexists
        // with other nodes having non-empty sets, so the implicit
        // backfill at `into_graph` time must respect explicit state.
        let reloaded_node = reloaded
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("gen"))
            .unwrap();
        assert!(
            !reloaded_node.exposed_params.contains("pattern"),
            "uncheck survives serde round-trip"
        );
    }

    #[test]
    fn toggle_node_param_expose_against_effect_flips_both_graph_and_user_binding() {
        // Project with one master effect using the catalog default.
        let mut project = Project::default();
        let effect_id = EffectId::new("test-mirror-instance");
        let mut fx = EffectInstance::new(EffectTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
        );

        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        // Graph side: exposed_params set carries the param.
        let def = fx.graph.as_ref().expect("graph lifted on first edit");
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(node.exposed_params.contains("rotation"));
        // Effect-side mirror: user_param_bindings appended because the
        // catalog default has no preset bindings for this param.
        assert_eq!(fx.user_param_bindings.len(), 1);
        assert_eq!(fx.user_param_bindings[0].node_handle, "uv_transform");
        assert_eq!(fx.user_param_bindings[0].inner_param, "rotation");

        // Undo reverses both sides.
        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let def = fx.graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(!node.exposed_params.contains("rotation"));
        assert_eq!(fx.user_param_bindings.len(), 0);
    }

    #[test]
    fn set_graph_node_param_against_generator_target_routes_to_layer() {
        let (mut project, lid) = project_with_one_generator_layer();
        let mut cmd = SetGraphNodeParamCommand::new(
            GraphTarget::Generator(lid.clone()),
            1, // uv_transform node id from mirror_catalog_default
            "rotation".to_string(),
            SerializedParamValue::Float { value: 45.0 },
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        let v = node.params.get("rotation").unwrap();
        match v {
            SerializedParamValue::Float { value } => assert!((value - 45.0).abs() < 1e-6),
            _ => panic!("expected Float param value"),
        }
    }
}
