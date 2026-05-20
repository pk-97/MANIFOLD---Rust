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
                output_formats: BTreeMap::new(),
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
        let new_value = self.new_value;
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
    /// Display label for the user binding (effect-side only).
    inner_label: String,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    /// Reverse state, populated on first execute(). See
    /// [`NodeExposeReverse`].
    reverse: NodeExposeReverse,
}

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
        /// Effect-side mirror reverse state. `None` for Generator
        /// targets; `Some` for Effect targets where the mirror ran.
        effect_mirror: Option<EffectMirrorReverse>,
    },
}

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
    /// with the captured slot value.
    RemovedUserBinding {
        binding: manifold_core::effects::UserParamBinding,
        position: usize,
        slot_value: manifold_core::effects::ParamSlot,
    },
    /// No-op: the Effect-side state already matched the requested
    /// state (idempotent re-toggle). Nothing to undo on the mirror.
    NoOp,
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
    ) -> Self {
        Self {
            target,
            node_handle,
            inner_param,
            expose,
            catalog_default,
            inner_meta: Some(inner_convert),
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

        // Graph-side write + locate static-block slot (if any). We
        // need a snapshot of the static-slot mapping BEFORE the
        // effect-side mutation, so do it inside this block.
        let graph_result = with_target_graph_mut(project, &self.target, &self.catalog_default, |def| {
            let prev_in_set =
                flip_graph_exposed(def, &node_handle, &inner_param, expose).unwrap_or(false);
            let static_slot = static_slot_for(def, &node_handle, &inner_param);
            (prev_in_set, static_slot)
        });

        let Some((prev_in_set, static_slot)) = graph_result else {
            // Target didn't resolve — nothing to undo.
            self.reverse = NodeExposeReverse::None;
            return;
        };

        // Effect-side mirror. Only runs for GraphTarget::Effect.
        let effect_mirror = match &self.target {
            GraphTarget::Effect(effect_id) => {
                let effect = match project.find_effect_by_id_mut(effect_id) {
                    Some(fx) => fx,
                    None => {
                        self.reverse = NodeExposeReverse::Captured {
                            prev_in_set,
                            effect_mirror: None,
                        };
                        return;
                    }
                };
                Some(mirror_effect_side(
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
                ))
            }
            GraphTarget::Generator(_) => None,
        };

        self.reverse = NodeExposeReverse::Captured {
            prev_in_set,
            effect_mirror,
        };
    }

    fn undo(&mut self, project: &mut Project) {
        let reverse = std::mem::take(&mut self.reverse);
        let NodeExposeReverse::Captured {
            prev_in_set,
            effect_mirror,
        } = reverse
        else {
            return;
        };

        // Effect-side restore first (must happen before the graph-side
        // restore if either fails for a deleted target, the effect
        // walk will short-circuit cleanly).
        if let (GraphTarget::Effect(effect_id), Some(mirror)) = (&self.target, effect_mirror)
            && let Some(effect) = project.find_effect_by_id_mut(effect_id)
        {
            unmirror_effect_side(effect, mirror);
        }

        // Graph-side restore. Use the non-lifting helper — if the
        // graph was None pre-execute we created it via the lifting
        // helper; on undo we leave whatever's there in place and just
        // restore the bit.
        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, |def| {
            restore_graph_exposed(def, &node_handle, &inner_param, prev_in_set);
        });
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
        let id = generate_unified_user_param_id(
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
        let _ = effect.remove_user_binding_by_id(&binding.id);
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            slot_value,
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
        }
    }
}

/// Mint a deterministic user_param_id for an effect's user-tail
/// binding. Mirrors the existing `generate_user_param_id` in
/// `crate::commands::effects` — we duplicate the logic here to avoid a
/// cross-module pub-item dependency from `graph.rs` into `effects.rs`.
fn generate_unified_user_param_id(
    inner_node_handle: &str,
    inner_param: &str,
    existing: &[manifold_core::effects::UserParamBinding],
) -> String {
    let short = inner_node_handle
        .rsplit_once('.')
        .map(|(_, t)| t)
        .unwrap_or(inner_node_handle);
    let prefix = format!("user.{short}.{inner_param}");
    let mut n: u32 = 1;
    loop {
        let candidate = format!("{prefix}.{n}");
        if !existing.iter().any(|b| b.id == candidate) {
            return candidate;
        }
        n += 1;
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
                    output_formats: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 1,
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    output_formats: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 2,
                    type_id: "node.mix".to_string(),
                    handle: Some("mix".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    output_formats: BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 3,
                    type_id: "system.final_output".to_string(),
                    handle: Some("final_output".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    output_formats: BTreeMap::new(),
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
                params: vec![ParamSpecDef {
                    id: "pattern".into(),
                    name: "Pattern".into(),
                    min: 0.0,
                    max: 7.0,
                    default_value: 0.0,
                    whole_numbers: true,
                    is_toggle: false,
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
                }],
                skip_mode: Default::default(),
                param_aliases: vec![],
                node_aliases: vec![],
                value_aliases: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.plasma_pattern_2d".to_string(),
                handle: Some("gen".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                output_formats: BTreeMap::new(),
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
