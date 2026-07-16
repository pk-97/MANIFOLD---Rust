//! Graph mutation commands — Phase 3 of per-card divergence,
//! generalized to support both effect graphs and generator graphs.
//!
//! Each command operates on the `EffectGraphDef` that a
//! [`manifold_core::GraphTarget`] points at. Targets resolve to:
//!
//! - [`GraphTarget::Effect`] → [`PresetInstance::graph`] with
//!   `PresetInstance::graph_version` as the version counter.
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
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    GroupDef, GroupInterface, InterfacePortDef, SerializedParamValue,
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
/// `structural` decides which version counter advances: `true` for an edit
/// that changes topology (node/wire add or remove) → bumps the structure
/// version → forces a chain rebuild; `false` for a value- or position-only edit
/// → bumps only the snapshot version → the renderer applies it in place with no
/// rebuild and no state reset.
fn with_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    structural: bool,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    project.with_preset_graph_mut(target, |host| {
        let def = host
            .graph_def_mut()
            .get_or_insert_with(|| catalog_default.clone());
        let r = f(def);
        if structural {
            host.bump_graph_structure_version();
        } else {
            host.bump_graph_version();
        }
        r
    })
}

/// Variant of [`with_target_graph_mut`] that doesn't lift the graph
/// from `None` — `f` only runs if the target already has a `Some(def)`.
/// Used by undo paths that mutate an already-edited graph; the catalog
/// default isn't needed because if the graph is `None` there's nothing
/// to undo.
fn with_existing_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    structural: bool,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    project
        .with_preset_graph_mut(target, |host| {
            let def = host.graph_def_mut().as_mut()?;
            let r = f(def);
            if structural {
                host.bump_graph_structure_version();
            } else {
                host.bump_graph_version();
            }
            Some(r)
        })
        .flatten()
}

/// Helper for the Revert command: take the target's current
/// `Option<EffectGraphDef>` (consuming it; leaves `None` in place) and
/// return what was there. Bumps the version counter.
fn take_target_graph(
    project: &mut Project,
    target: &GraphTarget,
) -> Option<Option<EffectGraphDef>> {
    project.with_preset_graph_mut(target, |host| {
        let prev = host.graph_def_mut().take();
        host.bump_graph_structure_version();
        prev
    })
}

/// Helper for the Revert command: install a given graph (or `None`)
/// at the target, bumping the version counter.
fn install_target_graph(
    project: &mut Project,
    target: &GraphTarget,
    graph: Option<EffectGraphDef>,
) {
    project.with_preset_graph_mut(target, |host| {
        *host.graph_def_mut() = graph;
        host.bump_graph_structure_version();
    });
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
    /// View depth this edit targets — a path of group ids (empty = document
    /// root). Lets the editor add nodes *inside* a group the user has
    /// descended into. See [`descend_level`].
    scope_path: Vec<u32>,
    /// `id` minted at first execute. Persisted across undo/redo so
    /// re-execute reuses the same id — downstream commands
    /// (`ConnectPorts`, `SetGraphNodeParam`) address by id.
    minted_id: Option<u32>,
    /// Stable `NodeId` minted at first execute. Persisted across undo/redo so
    /// a redo reuses the same identity — otherwise a binding made against this
    /// node would orphan when the node is re-created with a fresh id.
    minted_node_id: Option<NodeId>,
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
            scope_path: Vec::new(),
            minted_id: None,
            minted_node_id: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
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
        let scope = self.scope_path.clone();
        // Mint a stable identity once; reuse it on redo so a binding made
        // against this node survives undo/redo.
        let node_id = self
            .minted_node_id
            .clone()
            .unwrap_or_else(|| NodeId::new(manifold_core::short_id()));
        let node_id_for_store = node_id.clone();
        let minted = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let next_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            let id = prev_minted.unwrap_or(next_id);
            nodes.push(EffectGraphNode {
                id,
                node_id,
                type_id: node_type_id,
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: pos,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            });
            Some(id)
        })
        .flatten();
        match minted {
            Some(id) => {
                self.minted_id = Some(id);
                self.minted_node_id = Some(node_id_for_store);
            }
            None => eprintln!(
                "[manifold-editing] AddGraphNode: target {} / scope {:?} did not resolve",
                self.target.label(),
                self.scope_path
            ),
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(id) = self.minted_id else { return };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                nodes.retain(|n| n.id != id);
                wires.retain(|w| w.from_node != id && w.to_node != id);
            }
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
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
    /// Reverse state. `None` before first execute; populated to the
    /// removed node + its incident wires on success.
    removed: Option<RemovedNode>,
    /// Card sliders pruned because they were bound to the removed node
    /// (binding + spec + value slot + automation). Empty when the node backed
    /// no exposed params. Captured for undo; restored before the node is.
    removed_exposures: Vec<manifold_core::effects::RemovedExposure>,
}

#[derive(Debug, Clone)]
struct RemovedNode {
    node: EffectGraphNode,
    wires: Vec<EffectGraphWire>,
}

/// `node`'s own [`NodeId`] plus every descendant's, recursing into nested
/// groups (a group's `GroupDef.nodes` can itself contain group nodes). Used
/// by [`RemoveGraphNodeCommand`] so a group deletion prunes card-slider
/// exposures for its ENTIRE removed subtree, not just the group container's
/// own id (BUG-154) — a single-node removal is just the one-element case.
/// Empty `NodeId`s (anonymous boundary nodes) are skipped; they can't back
/// an exposure.
fn subtree_node_ids(node: &EffectGraphNode) -> Vec<NodeId> {
    let mut out = Vec::new();
    fn walk(node: &EffectGraphNode, out: &mut Vec<NodeId>) {
        if !node.node_id.is_empty() {
            out.push(node.node_id.clone());
        }
        if let Some(group) = &node.group {
            for child in &group.nodes {
                walk(child, out);
            }
        }
    }
    walk(node, &mut out);
    out
}

impl RemoveGraphNodeCommand {
    pub fn new(target: GraphTarget, node_id: u32, catalog_default: EffectGraphDef) -> Self {
        Self {
            target,
            node_id,
            catalog_default,
            scope_path: Vec::new(),
            removed: None,
            removed_exposures: Vec::new(),
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for RemoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_u32 = self.node_id;
        let scope = self.scope_path.clone();
        let catalog_default = &self.catalog_default;
        // One borrow of the instance: remove the node + wires, then prune any
        // card sliders bound to it. Done together so the whole thing is one
        // undoable unit and the slider can't outlive the node it drove.
        let captured = project
            .with_preset_graph_mut(&self.target, |inst| {
                let removed = {
                    let def = inst.graph.get_or_insert_with(|| catalog_default.clone());
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let node_pos = nodes.iter().position(|n| n.id == node_u32)?;
                    let node = nodes.remove(node_pos);
                    let removed_wires: Vec<EffectGraphWire> = wires
                        .iter()
                        .filter(|w| w.from_node == node_u32 || w.to_node == node_u32)
                        .cloned()
                        .collect();
                    wires.retain(|w| w.from_node != node_u32 && w.to_node != node_u32);
                    RemovedNode {
                        node,
                        wires: removed_wires,
                    }
                };
                // BUG-154: a removed GROUP node takes its entire nested
                // subgraph with it, but a card slider can be bound to a node
                // ANYWHERE inside that subgraph, not just the group container
                // itself. Pruning only `removed.node.node_id` left those
                // nested bindings dangling — the stale slider stayed on the
                // effect card with no warning after its node was gone.
                // Collect the whole removed subtree's node ids (self +
                // every descendant, recursing into nested groups) and prune
                // each — the same cleanup single-node deletion always got,
                // now applied uniformly regardless of removal shape.
                let mut removed_exposures = Vec::new();
                for nid in subtree_node_ids(&removed.node) {
                    removed_exposures.extend(inst.remove_exposures_for_node(&nid));
                }
                inst.bump_graph_structure_version();
                Some((removed, removed_exposures))
            })
            .flatten();
        if let Some((removed, removed_exposures)) = captured {
            self.removed = Some(removed);
            self.removed_exposures = removed_exposures;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(removed) = self.removed.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let removed_exposures = std::mem::take(&mut self.removed_exposures);
        project.with_preset_graph_mut(&self.target, move |inst| {
            if let Some(def) = inst.graph.as_mut()
                && let Some((nodes, wires)) =
                    descend_level(&mut def.nodes, &mut def.wires, &scope)
            {
                nodes.push(removed.node);
                wires.extend(removed.wires);
            }
            inst.restore_exposures(removed_exposures);
            inst.bump_graph_structure_version();
        });
    }

    fn description(&self) -> &str {
        "Remove Graph Node"
    }
}

/// Read-only: the display names of card sliders bound to the node whose runtime
/// id is `node_u32` at `scope_path` in `def`. Drives the delete-confirm dialog
/// (which sliders a node removal would take with it). Empty when the node backs
/// no exposed params. Clones `def` to reuse [`descend_level`]'s mutable walk —
/// a one-shot cost on a user-initiated delete, never a hot path.
pub fn exposed_param_labels_for_node(
    def: &EffectGraphDef,
    scope_path: &[u32],
    node_u32: u32,
) -> Vec<String> {
    use manifold_core::effect_graph_def::BindingTarget;
    let mut def = def.clone();
    let node_nid = {
        let Some((nodes, _)) = descend_level(&mut def.nodes, &mut def.wires, scope_path) else {
            return Vec::new();
        };
        match nodes.iter().find(|n| n.id == node_u32) {
            Some(n) => n.node_id.clone(),
            None => return Vec::new(),
        }
    };
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Vec::new();
    };
    meta.bindings
        .iter()
        .filter(|b| matches!(&b.target, BindingTarget::Node { node_id, .. } if *node_id == node_nid))
        // Only bindings that surface as a card slider (have a param spec).
        .filter_map(|b| meta.params.iter().find(|p| p.id == b.id).map(|p| p.name.clone()))
        .collect()
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
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
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
            scope_path: Vec::new(),
            displaced: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for ConnectPortsCommand {
    fn execute(&mut self, project: &mut Project) {
        let from_node = self.from_node;
        let from_port = self.from_port.clone();
        let to_node = self.to_node;
        let to_port = self.to_port.clone();
        let scope = self.scope_path.clone();
        let displaced =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (_nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let displaced = wires
                    .iter()
                    .position(|w| w.to_node == to_node && w.to_port == to_port)
                    .map(|i| wires.remove(i));
                wires.push(EffectGraphWire {
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
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let Some((_nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
            else {
                return;
            };
            if let Some(pos) = wires.iter().position(|w| {
                w.from_node == from_node
                    && w.from_port == from_port
                    && w.to_node == to_node
                    && w.to_port == to_port
            }) {
                wires.remove(pos);
            }
            if let Some(wire) = displaced {
                wires.push(wire);
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
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
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
            scope_path: Vec::new(),
            removed: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for DisconnectPortsCommand {
    fn execute(&mut self, project: &mut Project) {
        let to_node = self.to_node;
        let to_port = self.to_port.clone();
        let scope = self.scope_path.clone();
        let removed = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (_nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            wires
                .iter()
                .position(|w| w.to_node == to_node && w.to_port == to_port)
                .map(|pos| wires.remove(pos))
        })
        .flatten();
        self.removed = removed;
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(wire) = self.removed.take() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((_nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                wires.push(wire);
            }
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
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
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
            scope_path: Vec::new(),
            previous_pos: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for MoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        let new_pos = self.new_pos;
        let prev_already_captured = self.previous_pos.is_some();
        let scope = self.scope_path.clone();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let node = nodes.iter_mut().find(|n| n.id == node_id)?;
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
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == node_id)
            {
                node.editor_pos = previous;
            }
        });
    }

    fn description(&self) -> &str {
        "Move Graph Node"
    }
}

// ---------------------------------------------------------------------------
// Layout Graph Nodes (batch re-position)
// ---------------------------------------------------------------------------

/// `(node_id, prior editor_pos)` for one node, captured so a batch layout can
/// be undone. `editor_pos` is itself optional (a node may never have had a
/// stored position), hence the nested `Option`.
type NodePosBackup = (u32, Option<(f32, f32)>);

/// Re-position many nodes at once — the canvas "Tidy" command (Cmd+L), which
/// runs the layered auto-layout and ships every node's new position here. One
/// command so a tidy is a single undo step, not one per node. Previous
/// positions are captured on first `execute` for undo.
#[derive(Debug)]
pub struct LayoutGraphNodesCommand {
    target: GraphTarget,
    /// `(node_id, new_pos)` for every node at the targeted level.
    positions: Vec<(u32, (f32, f32))>,
    catalog_default: EffectGraphDef,
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
    /// Positions before execute(), for undo.
    previous: Option<Vec<NodePosBackup>>,
}

impl LayoutGraphNodesCommand {
    pub fn new(
        target: GraphTarget,
        positions: Vec<(u32, (f32, f32))>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            positions,
            catalog_default,
            scope_path: Vec::new(),
            previous: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for LayoutGraphNodesCommand {
    fn execute(&mut self, project: &mut Project) {
        let prev_already_captured = self.previous.is_some();
        let scope = self.scope_path.clone();
        let positions = self.positions.clone();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let mut prev = Vec::with_capacity(positions.len());
                for (node_id, new_pos) in &positions {
                    if let Some(node) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        prev.push((*node_id, node.editor_pos));
                        node.editor_pos = Some(*new_pos);
                    }
                }
                Some(prev)
            })
            .flatten();
        if !prev_already_captured && let Some(prev) = captured {
            self.previous = Some(prev);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous) = self.previous.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                for (node_id, prior) in &previous {
                    if let Some(node) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        node.editor_pos = *prior;
                    }
                }
            }
        });
    }

    fn description(&self) -> &str {
        "Tidy Graph Layout"
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
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
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
            scope_path: Vec::new(),
            previous_value: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for SetGraphNodeParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        let param_name = self.param_name.clone();
        let new_value = self.new_value.clone();
        let prev_already_captured = self.previous_value.is_some();
        let scope = self.scope_path.clone();
        // Closure return: `Option<SerializedParamValue>` — None if the
        // key didn't exist before the insert, Some(prev) if it did.
        // `with_target_graph_mut` wraps in another Option for target
        // resolution. `.flatten()` collapses: `None` here means the target
        // didn't resolve, the scope path didn't resolve, OR the node id
        // wasn't in the (descended) graph level.
        let captured: Option<Option<SerializedParamValue>> =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                nodes
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
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == node_id)
            {
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
// Set WGSL source (per-node kernel edit)
// ---------------------------------------------------------------------------

/// Replace a `node.wgsl_compute*` node's kernel source (`node.wgsl_source`).
/// The source is a real authoring surface — the graph editor's code panel
/// commits the whole edited buffer through here. Structural (the chain
/// recompiles the kernel); undo restores the prior source exactly, including
/// the `None` ("inherit the primitive's built-in WGSL") state.
#[derive(Debug)]
pub struct SetWgslSourceCommand {
    target: GraphTarget,
    node_id: u32,
    new_source: String,
    catalog_default: EffectGraphDef,
    /// View depth this edit targets (empty = root). See [`descend_level`].
    scope_path: Vec<u32>,
    /// Source before execute(). `Some(None)` means "node had no override
    /// source"; `Some(Some(s))` means "node had source `s`". `None` at
    /// pre-execute time.
    previous: Option<Option<String>>,
}

impl SetWgslSourceCommand {
    pub fn new(
        target: GraphTarget,
        node_id: u32,
        new_source: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            node_id,
            new_source,
            catalog_default,
            scope_path: Vec::new(),
            previous: None,
        }
    }

    /// Target a nested group level instead of the document root.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

impl Command for SetWgslSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id;
        // Empty buffer clears the override back to the primitive's built-in
        // kernel rather than compiling an empty shader.
        let new_source = if self.new_source.trim().is_empty() {
            None
        } else {
            Some(self.new_source.clone())
        };
        let prev_already_captured = self.previous.is_some();
        let scope = self.scope_path.clone();
        let captured: Option<Option<String>> =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                nodes.iter_mut().find(|n| n.id == node_id).map(|node| {
                    std::mem::replace(&mut node.wgsl_source, new_source.clone())
                })
            })
            .flatten();
        if !prev_already_captured && let Some(prev) = captured {
            self.previous = Some(prev);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous.take() else {
            return;
        };
        let node_id = self.node_id;
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == node_id)
            {
                node.wgsl_source = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Edit WGSL Source"
    }
}

// ---------------------------------------------------------------------------
// Revert Graph (effect or generator)
// ---------------------------------------------------------------------------

/// Clear the per-target graph override (either an `PresetInstance::graph`
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
    /// Automation orphaned by the revert — drivers / Ableton maps / envelopes
    /// that were hung on user-added params the cleared graph carried. Captured
    /// for undo; empty when the graph had no such params.
    removed_automation: manifold_core::effects::RemovedAutomation,
    /// User-added params the cleared graph carried, captured at their original
    /// display positions so undo can re-insert them exactly. Removing them is
    /// what makes `prune_orphaned_automation` see the driver's target gone and
    /// prune it (PARAM_STORAGE_DESIGN.md D3); without it the manifest still
    /// holds the orphaned param and the sweep is a no-op.
    removed_params: Vec<(usize, manifold_core::params::Param)>,
}

impl RevertEffectGraphCommand {
    pub fn new(target: GraphTarget) -> Self {
        Self {
            target,
            previous: None,
            removed_automation: Default::default(),
            removed_params: Vec::new(),
        }
    }
}

impl Command for RevertEffectGraphCommand {
    fn execute(&mut self, project: &mut Project) {
        let first = self.previous.is_none();
        if first {
            // First execute: capture and clear.
            self.previous = take_target_graph(project, &self.target);
        } else {
            // Re-execute (after undo): clear without re-capturing.
            install_target_graph(project, &self.target, None);
        }
        // The graph (and any user-added bindings it carried) is gone, so the
        // manifest's user-added params are now orphaned. Remove them BEFORE the
        // automation sweep — that is what makes `prune_orphaned_automation` see
        // the driver's target gone and prune it. Capture them at their original
        // positions so undo re-inserts exactly. Automation is captured once too.
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            if first {
                self.removed_params.clear();
                // Original positions first — removal shifts indices, so record
                // them before removing anything, then remove by id.
                let to_remove: Vec<(usize, String)> = inst
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.origin == manifold_core::params::ParamOrigin::UserAdded
                    })
                    .map(|(i, p)| (i, p.id().to_string()))
                    .collect();
                for (pos, id) in &to_remove {
                    if let Some(p) = inst.params.remove(id) {
                        self.removed_params.push((*pos, p));
                    }
                }
            } else {
                // Redo without an intervening undo: re-remove the same params.
                for (_, p) in &self.removed_params {
                    inst.params.remove(p.id());
                }
            }
            let pruned = inst.prune_orphaned_automation();
            if first {
                self.removed_automation = pruned;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous.take() else {
            return;
        };
        install_target_graph(project, &self.target, prev);
        let restored = std::mem::take(&mut self.removed_automation);
        let params = std::mem::take(&mut self.removed_params);
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            // Re-insert the removed params at their captured positions (ascending
            // order) before re-attaching automation.
            for (pos, p) in params {
                inst.params.insert_at(pos, p);
            }
            inst.restore_automation(restored);
        }
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
/// the legacy `PresetInstance.param_values[i].exposed` (for params
/// covered by a preset binding's static-block slot) and
/// [`PresetInstance::user_param_bindings`] (for inner-node params with
/// no preset binding). The mirror is what keeps the timeline-card
/// state-sync path working until Step 8 of the unification cuts those
/// fields over to the graph as the single source of truth.
///
/// For Generator targets, only the graph write happens — generators
/// never had a legacy `param_values` shadow.
#[derive(Debug)]
pub struct ToggleNodeParamExposeCommand {
    target: GraphTarget,
    /// Stable [`NodeId`] of the inner node — the identity the *mirror* side
    /// stores (the preset `BindingTarget::Node`, the `UserParamBinding.node_id`).
    /// NOT used to locate the node in the graph: it's empty on bundled-preset
    /// nodes, so the graph-side `exposed_params` write addresses by
    /// `(scope_path, node_u32_id)` instead — see [`Self::node_u32_id`].
    node_id: NodeId,
    /// Runtime (doc) id of the inner node, addressed at [`Self::scope_path`] —
    /// the same `(scope, id)` key every other graph command uses to reach a
    /// node (nested groups included). Always populated, so it locates the node
    /// where the stable `node_id` can't.
    node_u32_id: u32,
    /// View depth this edit targets — a path of group ids (empty = document
    /// root). Lets exposure reach a param on a node the user has descended
    /// into. See [`descend_level`].
    scope_path: Vec<u32>,
    /// Current display handle, used only to mint readable
    /// `user.<handle>.<param>.<n>` ids. Not an addressing role.
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
    /// Enum option labels for the inner param, captured at panel-build time
    /// from the live `ParamDef`. Flows onto the appended `UserParamBinding`
    /// (and its `ParamSpecDef`) so an exposed enum renders as a labelled
    /// stepped card slider instead of a bare numeric one. Empty for non-enums.
    inner_value_labels: Vec<String>,
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
        /// Mirror reverse state. Mirror collapse: effect and generator
        /// targets both run through `mirror_effect_side` over the target's
        /// `&mut PresetInstance` (the generator graph lives on `gen_params`),
        /// so there is one reverse type for both.
        mirror: EffectMirrorReverse,
    },
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
    /// The (handle, param) maps to a bundled-prefix param; we flipped its
    /// exposure via `set_param_exposed`. Undo restores `prev_exposed`.
    StaticSlot { param_id: String, prev_exposed: bool },
    /// The (handle, param) is a non-preset param; we appended a
    /// `UserParamBinding`. Undo removes it by id.
    AppendedUserBinding {
        user_param_id: String,
    },
    /// The (handle, param) is a non-preset param; we removed an
    /// existing `UserParamBinding`. Undo reinserts it at `position`
    /// with the captured manifest entry, plus re-attaches any orphaned
    /// drivers / Ableton mappings / envelopes that referenced the
    /// binding's id.
    RemovedUserBinding {
        binding: manifold_core::effects::UserParamBinding,
        position: usize,
        param: manifold_core::params::Param,
        /// Drivers pruned from `PresetInstance.drivers` because their
        /// `param_id` matched the removed binding's id. Without this
        /// pruning the rows would survive in the project file but
        /// never resolve to a target, leaving silently-dead
        /// automation behind.
        removed_drivers: Vec<manifold_core::effects::ParameterDriver>,
        /// Ableton mappings pruned for the same reason.
        removed_ableton_mappings:
            Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
        /// Envelopes pruned from `PresetInstance.envelopes` whose
        /// `param_id` matched the removed binding's id. Envelope-home
        /// unification put envelopes on the instance, so they prune and
        /// restore in the same effect borrow as drivers / Ableton
        /// mappings (no separate layer pass).
        removed_envelopes: Vec<manifold_core::effects::ParamEnvelope>,
    },
    /// No-op: the Effect-side state already matched the requested
    /// state (idempotent re-toggle). Nothing to undo on the mirror.
    NoOp,
}

impl ToggleNodeParamExposeCommand {
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        node_id: NodeId,
        node_u32_id: u32,
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
        inner_value_labels: Vec<String>,
    ) -> Self {
        Self {
            target,
            node_id,
            node_u32_id,
            scope_path: Vec::new(),
            node_handle,
            inner_param,
            expose,
            catalog_default,
            inner_meta: Some(inner_convert),
            inner_is_angle,
            inner_value_labels,
            inner_label,
            inner_min,
            inner_max,
            inner_default,
            reverse: NodeExposeReverse::None,
        }
    }

    /// Target a nested group level instead of the document root. Matches the
    /// `with_scope` builder every other graph command exposes.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

/// Flip `inner_param` membership in the `exposed_params` set of the node with
/// doc id `node_u32_id` within `nodes` (a single, already-descended graph
/// level). Returns the previous membership for undo, or `None` if the level has
/// no node with that id. Matches by the always-populated u32 doc id — the same
/// key `SetGraphNodeParamCommand` uses — because a bundled node's stable
/// `node_id` is empty and can't locate anything.
fn flip_node_exposed(
    nodes: &mut [EffectGraphNode],
    node_u32_id: u32,
    inner_param: &str,
    expose: bool,
) -> Option<bool> {
    let node = nodes.iter_mut().find(|n| n.id == node_u32_id)?;
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
/// Called by the expose command to materialise the implicit
/// preset-driven defaults before applying a user toggle. After the
/// first materialisation, `into_graph`'s binding backfill becomes a
/// no-op (it short-circuits when the def already carries explicit
/// exposure entries), so unchecks stick across save/reload.
fn materialize_binding_exposures(def: &mut EffectGraphDef) {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(meta) = def.preset_metadata.as_ref() else {
        return;
    };
    // Collect the (node_id, param) pairs first; we can't borrow meta
    // immutably while mutating nodes.
    let pairs: Vec<(NodeId, String)> = meta
        .bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::Node { node_id, param } => {
                Some((node_id.clone(), param.clone()))
            }
            BindingTarget::Composite { .. } => None,
        })
        .collect();
    for (node_id, param) in pairs {
        if let Some(node) = def.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.exposed_params.insert(param);
        }
    }
}

/// Restore `inner_param` membership in the `exposed_params` set of the node with
/// doc id `node_u32_id` within `nodes` (an already-descended level) to
/// `prev_in_set`. Idempotent — silently no-ops if the node is gone.
fn restore_node_exposed(
    nodes: &mut [EffectGraphNode],
    node_u32_id: u32,
    inner_param: &str,
    prev_in_set: bool,
) {
    if let Some(node) = nodes.iter_mut().find(|n| n.id == node_u32_id) {
        if prev_in_set {
            node.exposed_params.insert(inner_param.to_string());
        } else {
            node.exposed_params.remove(inner_param);
        }
    }
}

/// Find the static-block param slot index for a `(node_id, inner_param)`
/// pair, by scanning the preset metadata's bindings. Returns the
/// position in `metadata.params` of the binding whose target is
/// `(node_id, param)`. `None` if the def has no metadata or no binding
/// targets that `(node_id, param)`.
fn static_slot_for(
    def: &EffectGraphDef,
    node_id: &NodeId,
    inner_param: &str,
) -> Option<usize> {
    use manifold_core::effect_graph_def::BindingTarget;
    let meta = def.preset_metadata.as_ref()?;
    let binding_idx = meta.bindings.iter().position(|b| {
        // A user-added binding is NOT a static slot — it lives in the
        // user tail and is removed (not exposure-flipped) on unexpose.
        // Only bundled (shipped) bindings own a static `param_values` slot.
        if b.user_added {
            return false;
        }
        match &b.target {
            BindingTarget::Node { node_id: nid, param } => {
                nid == node_id && param == inner_param
            }
            BindingTarget::Composite { .. } => false,
        }
    })?;
    // Static-block slots are positional against `metadata.params` —
    // each `params[i]` corresponds to bindings sharing the same `id`.
    let binding_id = &meta.bindings[binding_idx].id;
    meta.params.iter().position(|p| &p.id == binding_id)
}

impl Command for ToggleNodeParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_handle = self.node_handle.clone();
        // Mirror-side identity for the card binding: apply the same "node_id
        // defaults to handle" convention the runtime graph loader uses
        // (`graph_loader.rs`), so a binding minted here targets the SAME
        // identity the chain resolves the node to. Bundled-preset nodes ship
        // with an empty stable `node_id`; without this the card slider would
        // bind to nothing and never drive the inner param. (The graph-side
        // `exposed_params` flip is located by `node_u32_id` below and doesn't
        // rely on this.)
        let node_id = if self.node_id.is_empty() {
            NodeId::new(node_handle.as_str())
        } else {
            self.node_id.clone()
        };
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let inner_label = self.inner_label.clone();
        let inner_min = self.inner_min;
        let inner_max = self.inner_max;
        let inner_default = self.inner_default;
        let inner_convert = self.inner_meta.unwrap_or(manifold_core::effects::ParamConvert::Float);
        let inner_is_angle = self.inner_is_angle;
        let inner_value_labels = self.inner_value_labels.clone();

        // Graph-side write — flip the node's `exposed_params` membership and
        // locate the static-block slot (if any). Identical for both kinds:
        // `with_target_graph_mut` resolves the target's graph (an effect's, or
        // a layer generator's `gen_params.graph`). The node is located by
        // `(scope_path, node_u32_id)` — `descend_level` walks into the group the
        // user is viewing, then matches the always-populated doc id — because a
        // bundled node's stable `node_id` is empty and won't locate anything.
        let scope = self.scope_path.clone();
        let node_u32_id = self.node_u32_id;
        let graph_result: Option<(bool, Option<usize>, Option<String>)> = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                // Materialise bundled binding exposures + resolve the static slot
                // at the def level (both read `preset_metadata`, which is
                // document-global), then descend to flip the target node.
                materialize_binding_exposures(def);
                let static_slot = static_slot_for(def, &node_id, &inner_param);
                // D5 section seed: resolve the innermost enclosing group's
                // display name from the ROOT nodes + scope_path BEFORE
                // descend_level narrows the borrow to the target level (an
                // immutable read; the &mut borrow below starts only after
                // this value is fully owned).
                let inner_section = innermost_group_display_name(&def.nodes, &scope);
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev_in_set = flip_node_exposed(nodes, node_u32_id, &inner_param, expose)?;
                Some((prev_in_set, static_slot, inner_section))
            },
        )
        .flatten();

        let Some((prev_in_set, static_slot, inner_section)) = graph_result else {
            // Target / scope / node didn't resolve — nothing to undo.
            self.reverse = NodeExposeReverse::None;
            return;
        };

        // Mirror collapse: both effect and generator targets run through
        // `mirror_effect_side` over the target's `&mut PresetInstance` — a
        // bundled param flips its `param_values[slot].exposed`, a user-added
        // param appends/removes a binding (and prunes its automation) via the
        // kind-aware `append_user_binding` / `remove_user_binding_by_id`. A
        // generator's `param_values[].exposed` bool is unread (its card shows
        // every graph param), so the bundled-slot flip is a harmless no-op
        // there; the real exposure is the `exposed_params` set flipped above.
        let instance: Option<&mut manifold_core::effects::PresetInstance> = match &self.target {
            GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
            GraphTarget::Generator(layer_id) => project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .map(|(_, layer)| layer.gen_params_or_init()),
        };
        let mirror = match instance {
            Some(inst) => mirror_effect_side(
                inst,
                &node_id,
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
                &inner_value_labels,
                inner_section,
            ),
            // Instance vanished between the graph borrow and the mirror borrow.
            // Capture just the graph bit so undo restores it.
            None => EffectMirrorReverse::NoOp,
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

        let inner_param = self.inner_param.clone();

        // Mirror collapse: restore the target's `&mut PresetInstance` through
        // `unmirror_effect_side` (binding + slot + automation, all in one
        // borrow now that envelopes ride on the instance), then restore the
        // graph `exposed_params` bit. Identical for both kinds.
        let instance: Option<&mut manifold_core::effects::PresetInstance> = match &self.target {
            GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
            GraphTarget::Generator(layer_id) => project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .map(|(_, layer)| layer.gen_params_or_init()),
        };
        if let Some(inst) = instance {
            unmirror_effect_side(inst, mirror);
        }
        let scope = self.scope_path.clone();
        let node_u32_id = self.node_u32_id;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                restore_node_exposed(nodes, node_u32_id, &inner_param, prev_in_set);
            }
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
/// Find the node `(node_id, node_handle)` addresses in `nodes` (recursing
/// into group bodies), using the same "empty stable id defaults to handle"
/// convention `ToggleNodeParamExposeCommand::execute` uses to mint the
/// identity in the first place: prefer a real `node_id` match; fall back to
/// `handle` only for a node whose own `node_id` is empty (a bundled node
/// that predates stable ids). D9's freeze-on-unmap write target.
fn find_node_by_id_or_handle_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
    node_handle: &str,
) -> Option<&'a mut EffectGraphNode> {
    let idx = nodes.iter().position(|n| {
        (!n.node_id.is_empty() && &n.node_id == node_id)
            || (n.node_id.is_empty() && n.handle.as_deref() == Some(node_handle))
    });
    if let Some(idx) = idx {
        return Some(&mut nodes[idx]);
    }
    for n in nodes.iter_mut() {
        if let Some(group) = n.group.as_deref_mut()
            && let Some(found) = find_node_by_id_or_handle_mut(&mut group.nodes, node_id, node_handle)
        {
            return Some(found);
        }
    }
    None
}

/// Convert an effective f32 value to the `SerializedParamValue` shape its
/// `ParamConvert` implies — the def-slot write shape for D9's freeze, mirror
/// of `param_binding::convert_param_value` (which targets the renderer-side
/// `ParamValue` instead of the wire `SerializedParamValue`).
fn effective_value_to_serialized(
    convert: manifold_core::effects::ParamConvert,
    value: f32,
) -> SerializedParamValue {
    use manifold_core::effects::ParamConvert;
    match convert {
        ParamConvert::Float | ParamConvert::Trigger => SerializedParamValue::Float { value },
        ParamConvert::IntRound => SerializedParamValue::Int { value: value.round() as i32 },
        ParamConvert::BoolThreshold => SerializedParamValue::Bool { value: value > 0.5 },
        ParamConvert::EnumRound => SerializedParamValue::Enum {
            value: value.round().max(0.0) as u32,
        },
    }
}

fn mirror_effect_side(
    effect: &mut manifold_core::effects::PresetInstance,
    node_id: &NodeId,
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
    inner_value_labels: &[String],
    // D5 (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): the innermost
    // enclosing group's display name, resolved by the caller from
    // `scope_path` BEFORE this fn runs. Only used on the append-new-binding
    // path (a static-slot toggle flips an EXISTING bundled spec, whose
    // section is whatever the preset author/importer set — expose never
    // overwrites it).
    inner_section: Option<String>,
) -> EffectMirrorReverse {
    use manifold_core::effects::UserParamBinding;

    if let Some(slot) = static_slot {
        // Bundled-prefix path: flip the exposure flag on the slot-th manifest
        // entry (bundled params occupy the prefix, in card order). Resolve the
        // positional slot to its stable id so undo re-addresses the same param.
        let Some(param_id) = effect.params.iter().nth(slot).map(|p| p.id().to_string())
        else {
            return EffectMirrorReverse::NoOp;
        };
        let prev_exposed = effect.is_param_exposed(&param_id);
        if prev_exposed == expose {
            return EffectMirrorReverse::NoOp;
        }
        effect.set_param_exposed(&param_id, expose);
        return EffectMirrorReverse::StaticSlot { param_id, prev_exposed };
    }

    // Non-static path: append / remove a user-added binding (stored in
    // the per-instance graph's `preset_metadata.bindings`).
    let user_bindings = effect.user_param_bindings();
    let existing_position = user_bindings
        .iter()
        .position(|b| &b.node_id == node_id && b.inner_param == inner_param);

    if expose {
        if existing_position.is_some() {
            return EffectMirrorReverse::NoOp;
        }
        let existing_ids: Vec<String> =
            user_bindings.iter().map(|b| b.id.clone()).collect();
        let id = crate::commands::effects::generate_user_param_id(
            node_handle,
            inner_param,
            &existing_ids,
        );
        let binding = UserParamBinding {
            id: id.clone(),
            label: inner_label.to_string(),
            node_id: node_id.clone(),
            legacy_node_handle: None,
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
            value_labels: inner_value_labels.to_vec(),
            section: inner_section,
        };
        effect.append_user_binding(binding);
        EffectMirrorReverse::AppendedUserBinding {
            user_param_id: id,
        }
    } else {
        let Some(position) = existing_position else {
            return EffectMirrorReverse::NoOp;
        };
        let binding = user_bindings[position].clone();
        let binding_id = binding.id.clone();
        // Capture the full manifest entry BEFORE removal so undo reinstates
        // the exact snapshot (value + base + calibration). The entry is
        // coupled to the binding by id (append/remove keep them in lockstep),
        // so there is no positional slot to compute — a generator instance
        // routes through here too (mirror collapse).
        let param = effect
            .params
            .get(&binding_id)
            .cloned()
            .expect("manifest entry present for a live user binding");
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
        let removed_envelopes = if let Some(es) = effect.envelopes.as_mut() {
            let mut taken = Vec::new();
            es.retain(|e| {
                let keep = e.param_id != binding_id;
                if !keep {
                    taken.push(e.clone());
                }
                keep
            });
            if es.is_empty() {
                effect.envelopes = None;
            }
            taken
        } else {
            Vec::new()
        };
        // D9 (`docs/PARAM_TWO_WAY_BINDING_DESIGN.md`): freeze the EFFECTIVE
        // value into the def slot this binding stops governing, so unmapping
        // never visually snaps the render to whatever stale value the slot
        // held from a pre-binding write. Must run BEFORE the binding is
        // removed (needs `binding`'s reshape) but after `param` is captured
        // above (needs its live `.value`).
        if let Some(graph) = effect.graph.as_mut() {
            let effective = manifold_core::effects::apply_card_reshape(
                param.value,
                binding.min,
                binding.max,
                binding.invert,
                binding.curve,
                binding.scale,
                binding.offset,
            );
            if let Some(target_node) =
                find_node_by_id_or_handle_mut(&mut graph.nodes, node_id, node_handle)
            {
                target_node.params.insert(
                    inner_param.to_string(),
                    effective_value_to_serialized(binding.convert, effective),
                );
            }
        }
        let _ = effect.remove_user_binding_by_id(&binding_id);
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            param,
            removed_drivers,
            removed_ableton_mappings,
            removed_envelopes,
        }
    }
}

fn unmirror_effect_side(
    effect: &mut manifold_core::effects::PresetInstance,
    mirror: EffectMirrorReverse,
) {
    match mirror {
        EffectMirrorReverse::NoOp => {}
        EffectMirrorReverse::StaticSlot { param_id, prev_exposed } => {
            effect.set_param_exposed(&param_id, prev_exposed);
        }
        EffectMirrorReverse::AppendedUserBinding { user_param_id } => {
            let _ = effect.remove_user_binding_by_id(&user_param_id);
        }
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            param,
            removed_drivers,
            removed_ableton_mappings,
            removed_envelopes,
        } => {
            // Restore the binding (graph metadata + reshape note) and its
            // manifest entry at the original tail position so other user
            // bindings keep their card order.
            effect.restore_user_binding_at(binding, position, param);
            // Restore the automation rows that referenced this binding.
            // The same id resolves through the manifest again since we
            // re-inserted the binding above.
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
            if !removed_envelopes.is_empty() {
                effect
                    .envelopes
                    .get_or_insert_with(Vec::new)
                    .extend(removed_envelopes);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Group / Ungroup
// ---------------------------------------------------------------------------

/// Navigate to the node + wire vectors of the sub-graph at `scope` — a list of
/// group-node ids to descend into (empty = the document root). Returns `None`
/// if a hop doesn't resolve to a group. The mutable handles let a command both
/// read the level (snapshot for undo) and replace it (apply the transform).
fn descend_level<'a>(
    nodes: &'a mut Vec<EffectGraphNode>,
    wires: &'a mut Vec<EffectGraphWire>,
    scope: &[u32],
) -> Option<(&'a mut Vec<EffectGraphNode>, &'a mut Vec<EffectGraphWire>)> {
    match scope.split_first() {
        None => Some((nodes, wires)),
        Some((gid, rest)) => {
            let group = nodes.iter_mut().find(|n| n.id == *gid)?;
            let body = group.group.as_deref_mut()?;
            descend_level(&mut body.nodes, &mut body.wires, rest)
        }
    }
}

/// Resolve the display name (`handle`) of the innermost group named by
/// `scope` — the group whose name an exposed param's card `section` is
/// stamped with (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D5). `scope` is a
/// path of group-node ids from the document root; the LAST id is the
/// innermost group. Returns `None` for a top-level node (empty scope), or if
/// any hop doesn't resolve to a named group (an anonymous boundary node has
/// `handle: None` — matches D5's "top-level nodes get `None`" for that edge
/// case too, rather than a panic).
fn innermost_group_display_name(nodes: &[EffectGraphNode], scope: &[u32]) -> Option<String> {
    let mut level = nodes;
    let mut name = None;
    for gid in scope {
        let node = level.iter().find(|n| n.id == *gid)?;
        name = node.handle.clone();
        level = node.group.as_deref()?.nodes.as_slice();
    }
    name
}

/// Collect every populated stable [`NodeId`] within `nodes` and all nested
/// group bodies, at any depth — used by `RenameGroupCommand`'s D5
/// section-sweep to test "does this binding's target live inside the group
/// we just renamed." Includes nested groups' own ids (a binding could in
/// principle target a group node directly), not just leaves.
fn collect_node_ids(nodes: &[EffectGraphNode], out: &mut Vec<NodeId>) {
    for n in nodes {
        if !n.node_id.is_empty() {
            out.push(n.node_id.clone());
        }
        if let Some(body) = n.group.as_deref() {
            collect_node_ids(&body.nodes, out);
        }
    }
}

/// Collapse a selection at `scope_path` into a single group node, via
/// [`manifold_core::group_edit::group_selection`]. Undo restores the level
/// wholesale (a structural transform touches many nodes/wires, so a level
/// snapshot is the clean reverse).
#[derive(Debug)]
pub struct GroupNodesCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    selected: Vec<u32>,
    handle: String,
    centroid: (f32, f32),
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before collapse. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl GroupNodesCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        selected: Vec<u32>,
        handle: String,
        centroid: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            selected,
            handle,
            centroid,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for GroupNodesCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let selected: std::collections::BTreeSet<u32> = self.selected.iter().copied().collect();
        let handle = self.handle.clone();
        let centroid = self.centroid;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());
            match manifold_core::group_edit::group_selection(
                nodes.clone(),
                wires.clone(),
                &selected,
                &handle,
                centroid,
            ) {
                Ok((nn, nw)) => {
                    *nodes = nn;
                    *wires = nw;
                    Some(prev)
                }
                Err(e) => {
                    eprintln!("[manifold-editing] GroupNodes: {e:?}");
                    None
                }
            }
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Group Nodes"
    }
}

/// Dissolve a group node at `scope_path` back into its level, via
/// [`manifold_core::group_edit::ungroup`]. The inverse of [`GroupNodesCommand`].
#[derive(Debug)]
pub struct UngroupNodeCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl UngroupNodeCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for UngroupNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let group_node_id = self.group_node_id;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());
            match manifold_core::group_edit::ungroup(nodes.clone(), wires.clone(), group_node_id) {
                Ok((nn, nw)) => {
                    *nodes = nn;
                    *wires = nw;
                    Some(prev)
                }
                Err(e) => {
                    eprintln!("[manifold-editing] UngroupNode: {e:?}");
                    None
                }
            }
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Ungroup Node"
    }
}

// ---------------------------------------------------------------------------
// Set group tint (cosmetic, non-structural)
// ---------------------------------------------------------------------------

/// Set (or clear) the accent colour of a group node at `scope_path`. Cosmetic
/// only — it never changes what runs, so it routes as a non-structural edit
/// (no chain rebuild). Undo restores the prior tint.
#[derive(Debug)]
pub struct SetGroupTintCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    tint: Option<[f32; 4]>,
    catalog_default: EffectGraphDef,
    /// Pre-edit tint. `Some(prev)` once captured; outer `Option` distinguishes
    /// "not yet executed."
    prev: Option<Option<[f32; 4]>>,
}

impl SetGroupTintCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        tint: Option<[f32; 4]>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            tint,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for SetGroupTintCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let tint = self.tint;
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let group = nodes
                    .iter_mut()
                    .find(|n| n.id == id)
                    .and_then(|n| n.group.as_mut())?;
                let prev = group.tint;
                group.tint = tint;
                Some(prev)
            });
        if self.prev.is_none() {
            self.prev = captured.flatten();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev else {
            return;
        };
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(group) = nodes
                    .iter_mut()
                    .find(|n| n.id == id)
                    .and_then(|n| n.group.as_mut())
            {
                group.tint = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Group Tint"
    }
}

// ---------------------------------------------------------------------------
// Add Scene Object / Add Scene Light
// (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D7/D7a, P5)
// ---------------------------------------------------------------------------

/// Build a plain (non-group, non-boundary) node for the scene-build gestures
/// below — same 12-field shape `AddGraphNodeCommand`/`group_edit::group_selection`
/// use, factored out so the two commands below don't repeat the struct literal
/// four times.
fn scene_build_node(id: u32, type_id: &str, handle: Option<String>, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(manifold_core::short_id()),
        type_id: type_id.to_string(),
        handle,
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn scene_build_wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// The add-object gesture (D7): one undoable composite edit that (1) bumps
/// `render_scene`'s `objects` count by one, (2) builds a new group named
/// "Object N" containing a placeholder `node.cube_mesh` + a tinted
/// `node.phong_material` + a `node.transform_3d`, wired to a
/// `system.group_output` boundary exposing `vertices`/`material`/`transform`,
/// (3) wires the group's three outputs to the new `mesh_k`/`material_k`/
/// `transform_k` ports on `render_scene`. Mirrors `GroupNodesCommand`'s
/// whole-level snapshot/restore shape — this is a structural composite edit
/// exactly like a group-creation, so undo restores the pre-edit `(nodes,
/// wires)` verbatim rather than reversing each sub-step by hand.
///
/// `next_index` (the new object's 0-based slot, `k` in `mesh_k`/`material_k`/
/// `transform_k`) is resolved by the caller from the LIVE `objects` param
/// value shown on the node face at click time — not re-derived here. This
/// command can't fall back on `render_scene`'s own `DEFAULT_OBJECTS`/
/// `OBJECT_SAFETY_MAX` (they're private to `manifold-renderer`, which
/// `manifold-editing` does not depend on), so the UI's already-resolved count
/// is the one source of truth; `execute()` is a deterministic function of it.
#[derive(Debug)]
pub struct AddSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    next_index: u32,
    centroid: (f32, f32),
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl AddSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        centroid: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            next_index,
            centroid,
            catalog_default,
            prev: None,
        }
    }
}

/// A distinct RGBA tint for object slot `k`, spread around the hue wheel by
/// the golden ratio at high saturation — the SAME formula
/// `gltf_import.rs::group_tint` uses for imported objects (that fn is private
/// to `manifold-renderer`, unreachable from here, so this is a same-formula
/// re-derivation, not a shared call — keep the two in sync if either changes).
/// So an added cube reads as one more colour-coded box beside imported ones,
/// never a jarring one-off.
fn scene_object_tint(k: u32) -> manifold_core::Color {
    let hue = (k as f32 * 0.618_034) % 1.0;
    manifold_core::Color::hsv_to_rgb(hue, 0.7, 0.85)
}

impl Command for AddSceneObjectCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let centroid = self.centroid;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            nodes
                .iter_mut()
                .find(|n| n.id == render_id)?
                .params
                .insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: (k + 1) as f32,
                    },
                );

            let mut next_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            let mut fresh = move || {
                let v = next_id;
                next_id += 1;
                v
            };
            let mesh_id = fresh();
            let mat_id = fresh();
            let transform_id = fresh();
            let out_id = fresh();
            let group_id = fresh();

            let tint = scene_object_tint(k);
            let mut mat_params = BTreeMap::new();
            mat_params.insert("color_r".to_string(), SerializedParamValue::Float { value: tint.r });
            mat_params.insert("color_g".to_string(), SerializedParamValue::Float { value: tint.g });
            mat_params.insert("color_b".to_string(), SerializedParamValue::Float { value: tint.b });

            let mesh_node = scene_build_node(mesh_id, "node.cube_mesh", Some(format!("mesh_{k}")), BTreeMap::new());
            let mat_node = scene_build_node(mat_id, "node.phong_material", Some(format!("mat_{k}")), mat_params);
            let transform_node = scene_build_node(
                transform_id,
                "node.transform_3d",
                Some(format!("transform_{k}")),
                BTreeMap::new(),
            );
            let out_node = scene_build_node(out_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());

            let group_wires = vec![
                scene_build_wire(mesh_id, "vertices", out_id, "vertices"),
                scene_build_wire(mat_id, "out", out_id, "material"),
                scene_build_wire(transform_id, "transform", out_id, "transform"),
            ];

            let mut group_node = scene_build_node(
                group_id,
                GROUP_TYPE_ID,
                Some(format!("Object {}", k + 1)),
                BTreeMap::new(),
            );
            group_node.editor_pos = Some(centroid);
            group_node.group = Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: Vec::new(),
                    outputs: vec![
                        InterfacePortDef {
                            name: "vertices".to_string(),
                            port_type: "Array(Vertex)".to_string(),
                        },
                        InterfacePortDef {
                            name: "material".to_string(),
                            port_type: "Material".to_string(),
                        },
                        InterfacePortDef {
                            name: "transform".to_string(),
                            port_type: "Transform".to_string(),
                        },
                    ],
                    params: Vec::new(),
                },
                nodes: vec![mesh_node, mat_node, transform_node, out_node],
                wires: group_wires,
                tint: Some([tint.r, tint.g, tint.b, 1.0]),
            }));

            nodes.push(group_node);
            wires.push(scene_build_wire(group_id, "vertices", render_id, &format!("mesh_{k}")));
            wires.push(scene_build_wire(group_id, "material", render_id, &format!("material_{k}")));
            wires.push(scene_build_wire(group_id, "transform", render_id, &format!("transform_{k}")));

            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Add Object"
    }
}

/// The add-light gesture (D7a): one undoable composite edit that (1) bumps
/// `render_scene`'s `lights` count by one, (2) spawns a BARE `node.light`
/// (no group — a one-node group taxes every future edit for zero legibility,
/// D7a's explicit ruling) named "Light N", (3) auto-wires its `out` into the
/// new `light_k` port. Defaults transcribed from D7a: Sun, white, intensity
/// 1.0, ~45° elevation, `cast_shadows` ON. Same whole-level snapshot/restore
/// shape as `AddSceneObjectCommand` / `GroupNodesCommand`.
#[derive(Debug)]
pub struct AddSceneLightCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    next_index: u32,
    pos: (f32, f32),
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl AddSceneLightCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            next_index,
            pos,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for AddSceneLightCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            nodes
                .iter_mut()
                .find(|n| n.id == render_id)?
                .params
                .insert(
                    "lights".to_string(),
                    SerializedParamValue::Float {
                        value: (k + 1) as f32,
                    },
                );

            let light_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            // D7a defaults, transcribed from `node.light`'s own param defs
            // (`crates/manifold-renderer/src/node_graph/primitives/light.rs`):
            // mode=Sun / color white / intensity 1.0 / cast_shadows ON already
            // match the primitive's own defaults — set explicitly anyway so
            // the gesture's contract doesn't silently drift if those defaults
            // ever change. pos is overridden for ~45° elevation (the
            // primitive's own default is pos_y=30 with pos_x=pos_z=0, i.e.
            // straight overhead, which flattens the scene); aim stays at the
            // primitive's (0,0,0) default.
            let mut params = BTreeMap::new();
            params.insert("mode".to_string(), SerializedParamValue::Enum { value: 0 }); // Sun
            params.insert("pos_x".to_string(), SerializedParamValue::Float { value: 0.0 });
            params.insert("pos_y".to_string(), SerializedParamValue::Float { value: 7.0 });
            params.insert("pos_z".to_string(), SerializedParamValue::Float { value: 7.0 });
            params.insert("color_r".to_string(), SerializedParamValue::Float { value: 1.0 });
            params.insert("color_g".to_string(), SerializedParamValue::Float { value: 1.0 });
            params.insert("color_b".to_string(), SerializedParamValue::Float { value: 1.0 });
            params.insert("intensity".to_string(), SerializedParamValue::Float { value: 1.0 });
            params.insert("cast_shadows".to_string(), SerializedParamValue::Float { value: 1.0 });

            let mut light_node = scene_build_node(
                light_id,
                "node.light",
                Some(format!("light_{k}")),
                params,
            );
            light_node.editor_pos = Some(pos);
            nodes.push(light_node);
            wires.push(scene_build_wire(light_id, "out", render_id, &format!("light_{k}")));

            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Add Light"
    }
}

// ---------------------------------------------------------------------------
// Add Scene Environment / Add Scene Fog
// (SCENE_SETUP_PANEL_DESIGN.md D3/D4, P1) — shaped exactly like
// AddSceneLightCommand above: spawn one new node at the scene's graph level
// and wire it straight into the render_scene port the Vm found unwired.
// The panel only ever offers these actions when `EnvironmentVm::None` /
// `AtmosphereVm::None` (D3), so neither command needs to guard against an
// already-wired port — same non-guarding posture AddSceneLightCommand takes
// for `lights`.
// ---------------------------------------------------------------------------

/// "Add environment" (D3): spawn a `node.bake_environment` at the scene's
/// graph level and wire its `envmap` output into `render_scene`'s `envmap`
/// input. One undo unit.
#[derive(Debug)]
pub struct AddSceneEnvironmentCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    pos: (f32, f32),
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl AddSceneEnvironmentCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self { target, scope_path, render_scene_node_id, pos, catalog_default, prev: None }
    }
}

impl Command for AddSceneEnvironmentCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let env_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            // Primitive defaults (`node.bake_environment`) match the importer's
            // OWN softbox default (F-P4) so a freshly-added environment reads
            // as a sane, lit studio rather than a black void — explicit here
            // anyway so the gesture's contract doesn't silently drift if the
            // primitive's defaults ever change.
            let mut params = BTreeMap::new();
            params.insert("mode".to_string(), SerializedParamValue::Enum { value: 1 }); // Softbox
            params.insert("intensity".to_string(), SerializedParamValue::Float { value: 1.0 });
            params.insert("fill".to_string(), SerializedParamValue::Float { value: 0.0 });

            let mut env_node =
                scene_build_node(env_id, "node.bake_environment", Some("environment".to_string()), params);
            env_node.editor_pos = Some(pos);
            nodes.push(env_node);
            wires.push(scene_build_wire(env_id, "envmap", render_id, "envmap"));

            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Add Environment"
    }
}

/// "Add fog" (D3): spawn a `node.atmosphere` at the scene's graph level and
/// wire its `atmosphere` output into `render_scene`'s `atmosphere` input.
/// One undo unit.
#[derive(Debug)]
pub struct AddSceneFogCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    pos: (f32, f32),
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl AddSceneFogCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self { target, scope_path, render_scene_node_id, pos, catalog_default, prev: None }
    }
}

impl Command for AddSceneFogCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let fog_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            // A freshly-added fog node starts at density 0 (the primitive's own
            // default — "subtle" is authored by hand in the starter preset, not
            // stamped here) so adding it is never a visible surprise; the
            // performer dials density up from the panel immediately after.
            let params = BTreeMap::new();

            let mut fog_node = scene_build_node(fog_id, "node.atmosphere", Some("fog".to_string()), params);
            fog_node.editor_pos = Some(pos);
            nodes.push(fog_node);
            wires.push(scene_build_wire(fog_id, "atmosphere", render_id, "atmosphere"));

            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Add Fog"
    }
}

// ---------------------------------------------------------------------------
// Rename group (handle = namespace, so structural)
// ---------------------------------------------------------------------------

/// Rename a group node at `scope_path`. The handle is the group's namespace
/// (it prefixes inner handles at flatten time), so this is a structural edit.
/// Rejected as a no-op when the new handle is empty, contains `/`, or collides
/// with a sibling at the same level. Undo restores the prior handle.
#[derive(Debug)]
pub struct RenameGroupCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    new_handle: String,
    catalog_default: EffectGraphDef,
    /// Pre-edit handle. `Some(prev)` once captured (the rename was applied);
    /// stays `None` when the rename was rejected or never executed.
    prev: Option<Option<String>>,
    /// D5 rename-sweep undo state (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2):
    /// `(param_id, prior_section)` for every card spec whose `section`
    /// followed this rename (it equaled the OLD group name and its binding
    /// target resolved inside the renamed group). Empty when nothing
    /// matched, or on a rejected/no-op rename. A hand-edited section (any
    /// other string) never lands here — it's untouched by the sweep.
    swept: Vec<(String, Option<String>)>,
}

impl RenameGroupCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        new_handle: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            new_handle,
            catalog_default,
            prev: None,
            swept: Vec::new(),
        }
    }

    /// Resolve the `&mut PresetInstance` this command's `target` addresses —
    /// same match every `graph.rs` command uses (mirrors
    /// `ToggleNodeParamExposeCommand`'s identical resolve for its mirror
    /// step). Used by the D5 sweep, which needs the manifest (`.params`)
    /// alongside the graph — outside `with_target_graph_mut`'s narrower
    /// `&mut EffectGraphDef` view.
    fn resolve_instance<'p>(
        &self,
        project: &'p mut Project,
    ) -> Option<&'p mut manifold_core::effects::PresetInstance> {
        match &self.target {
            GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
            GraphTarget::Generator(layer_id) => project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .map(|(_, layer)| layer.gen_params_or_init()),
        }
    }
}

impl Command for RenameGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let new_handle = self.new_handle.clone();
        // Guard against a repeated execute() (e.g. a defensive double-call
        // with no intervening undo) re-deriving `prev`/re-sweeping from an
        // already-renamed state — same guard shape the original code used
        // for `self.prev` alone.
        let first_time = self.prev.is_none();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                // Reject invalid / colliding names — a rejected rename changes
                // nothing (the canvas keeps the old name).
                if new_handle.is_empty() || new_handle.contains('/') {
                    return None;
                }
                if nodes
                    .iter()
                    .any(|n| n.id != id && n.handle.as_deref() == Some(new_handle.as_str()))
                {
                    return None;
                }
                let node = nodes.iter_mut().find(|n| n.id == id)?;
                // Only groups carry a renamable namespace here.
                node.group.as_ref()?;
                let prev = node.handle.clone();
                node.handle = Some(new_handle.clone());
                // D5 sweep prep: every stable NodeId inside the renamed
                // group's subtree (any depth) — the "does this binding
                // target live inside the group we just renamed" test below.
                let mut inside = Vec::new();
                if let Some(body) = node.group.as_deref() {
                    collect_node_ids(&body.nodes, &mut inside);
                }
                Some((prev, inside))
            });
        let Some((prev, inside)) = captured.flatten() else {
            return;
        };
        if first_time {
            self.prev = Some(prev.clone());
        }
        if !first_time {
            // Sweep already ran on the genuine first execute; a repeated
            // call is a no-op past the handle write above.
            return;
        }

        // D5 rename-sweep: any card spec whose `section` equals the OLD
        // group name AND whose binding target resolves inside the renamed
        // group follows the rename — one undoable command, both writes.
        let Some(old_name) = prev else {
            // The group had no name before this rename — nothing could have
            // been sectioned under it.
            return;
        };
        let Some(inst) = self.resolve_instance(project) else {
            return;
        };
        let target_ids: Vec<String> = inst
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .map(|m| {
                m.bindings
                    .iter()
                    .filter(|b| match &b.target {
                        manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                            inside.contains(node_id)
                        }
                        manifold_core::effect_graph_def::BindingTarget::Composite { .. } => false,
                    })
                    .map(|b| b.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.swept.clear();
        for param_id in target_ids {
            if let Some(p) = inst.params.get_mut(&param_id)
                && p.spec.section.as_deref() == Some(old_name.as_str())
            {
                self.swept.push((param_id, p.spec.section.clone()));
                p.spec.section = Some(new_handle.clone());
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.swept.is_empty()
            && let Some(inst) = self.resolve_instance(project)
        {
            for (param_id, prev_section) in self.swept.drain(..) {
                if let Some(p) = inst.params.get_mut(&param_id) {
                    p.spec.section = prev_section;
                }
            }
        }

        let Some(prev) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == id)
            {
                node.handle = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Rename Group"
    }
}

// ---------------------------------------------------------------------------
// Paste nodes (copy/paste/duplicate within a graph level)
// ---------------------------------------------------------------------------

/// Paste a set of copied nodes (and the wires among them) into the level at
/// `scope_path`. Each pasted node gets a fresh runtime id, a fresh stable
/// `NodeId`, a deduped handle, and an editor-position offset, so a copy never
/// collides with its source. A wire whose both endpoints are in the copied set
/// is re-anchored to the new ids; external wires are dropped (paste carries
/// internal connectivity only). Structural (the chain rebuilds); undo removes
/// exactly the pasted nodes and wires. Backs Cmd+V (paste) and copy-then-paste
/// duplication.
#[derive(Debug)]
pub struct PasteNodesCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    src_nodes: Vec<EffectGraphNode>,
    src_wires: Vec<EffectGraphWire>,
    offset: (f32, f32),
    catalog_default: EffectGraphDef,
    /// Minted on first execute, reused on redo so a pasted node's identity (and
    /// any binding later made against it) survives undo/redo: `(src id, new id,
    /// new node_id)`.
    remap: Option<Vec<(u32, u32, NodeId)>>,
}

impl PasteNodesCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        src_nodes: Vec<EffectGraphNode>,
        src_wires: Vec<EffectGraphWire>,
        offset: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            src_nodes,
            src_wires,
            offset,
            catalog_default,
            remap: None,
        }
    }
}

/// `base`, else `base_2`, `base_3`, … — the first form not already in `taken`.
/// Inserts the chosen handle into `taken` so a batch paste stays collision-free.
fn dedup_handle(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if !taken.contains(base) {
        taken.insert(base.to_string());
        return base.to_string();
    }
    let mut i = 2u32;
    loop {
        let cand = format!("{base}_{i}");
        if !taken.contains(&cand) {
            taken.insert(cand.clone());
            return cand;
        }
        i += 1;
    }
}

impl Command for PasteNodesCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let existing_remap = self.remap.clone();
        let src_nodes = &self.src_nodes;
        let src_wires = &self.src_wires;
        let offset = self.offset;
        let result = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                // Fresh ids start past the level's current max; fresh node_ids
                // are minted once and reused on redo.
                let mut next_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                let remap: Vec<(u32, u32, NodeId)> = existing_remap.unwrap_or_else(|| {
                    src_nodes
                        .iter()
                        .map(|sn| {
                            let new_id = next_id;
                            next_id += 1;
                            (sn.id, new_id, NodeId::new(manifold_core::short_id()))
                        })
                        .collect()
                });
                let mut taken: std::collections::HashSet<String> =
                    nodes.iter().filter_map(|n| n.handle.clone()).collect();
                for sn in src_nodes {
                    let Some((_, new_id, new_node_id)) =
                        remap.iter().find(|(orig, _, _)| *orig == sn.id)
                    else {
                        continue;
                    };
                    let mut node = sn.clone();
                    node.id = *new_id;
                    node.node_id = new_node_id.clone();
                    node.handle = sn.handle.as_deref().map(|h| dedup_handle(h, &mut taken));
                    node.editor_pos = Some(match sn.editor_pos {
                        Some((x, y)) => (x + offset.0, y + offset.1),
                        None => offset,
                    });
                    // The copy isn't card-exposed (its outer bindings address the
                    // original by node_id); start it un-exposed so no binding dangles.
                    node.exposed_params = Default::default();
                    nodes.push(node);
                }
                for sw in src_wires {
                    let from = remap.iter().find(|(o, _, _)| *o == sw.from_node);
                    let to = remap.iter().find(|(o, _, _)| *o == sw.to_node);
                    if let (Some((_, fid, _)), Some((_, tid, _))) = (from, to) {
                        wires.push(EffectGraphWire {
                            from_node: *fid,
                            from_port: sw.from_port.clone(),
                            to_node: *tid,
                            to_port: sw.to_port.clone(),
                        });
                    }
                }
                Some(remap)
            },
        )
        .flatten();
        if self.remap.is_none() {
            self.remap = result;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(remap) = self.remap.clone() else {
            return;
        };
        let new_ids: std::collections::HashSet<u32> =
            remap.iter().map(|(_, n, _)| *n).collect();
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                nodes.retain(|n| !new_ids.contains(&n.id));
                wires.retain(|w| {
                    !new_ids.contains(&w.from_node) && !new_ids.contains(&w.to_node)
                });
            }
        });
    }

    fn description(&self) -> &str {
        "Paste Nodes"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::EffectId;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
    use manifold_core::effects::PresetInstance;

    fn slot(id: &str, value: f32, exposed: bool) -> manifold_core::params::Param {
        let mut p = manifold_core::params::Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        });
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

    // ── node groups: group / ungroup commands ──

    fn abc_graph() -> EffectGraphDef {
        let mk = |id: u32, handle: &str, ty: &str| EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(handle),
            type_id: ty.to_string(),
            handle: Some(handle.to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let w = |fln: u32, fp: &str, tn: u32, tp: &str| EffectGraphWire {
            from_node: fln,
            from_port: fp.to_string(),
            to_node: tn,
            to_port: tp.to_string(),
        };
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                mk(0, "a", "system.source"),
                mk(1, "b", "node.transform"),
                mk(2, "c", "system.final_output"),
            ],
            wires: vec![w(0, "out", 1, "in"), w(1, "out", 2, "in")],
        }
    }

    fn project_with_graph(def: EffectGraphDef) -> (Project, EffectId) {
        let mut project = Project::default();
        let effect_id = EffectId::new("test-group-fx");
        let mut fx = PresetInstance::new(PresetTypeId::new("test.fx"));
        fx.id = effect_id.clone();
        fx.graph = Some(def);
        project.settings.master_effects.push(fx);
        (project, effect_id)
    }

    fn graph_of<'a>(project: &'a Project, id: &EffectId) -> &'a EffectGraphDef {
        project
            .find_effect_by_id(id)
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
    }

    #[test]
    fn group_nodes_command_collapses_and_undo_restores() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let mut cmd = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (5.0, 6.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let g = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .expect("group node created");
        assert!(g.group.is_some());
        assert_eq!(g.editor_pos, Some((5.0, 6.0)));
        assert!(
            !def.nodes.iter().any(|n| n.handle.as_deref() == Some("b")),
            "b moved into the group"
        );
        let body = g.group.as_deref().unwrap();
        assert!(body.nodes.iter().any(|n| n.handle.as_deref() == Some("b")));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(
            def.nodes.iter().any(|n| n.handle.as_deref() == Some("b")),
            "b restored at top level"
        );
        assert!(
            !def.nodes.iter().any(|n| n.handle.as_deref() == Some("g")),
            "group node removed"
        );
    }

    #[test]
    fn ungroup_command_inverts_group_then_undo_restores() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        let mut ungroup = UngroupNodeCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            g_id,
            mirror_catalog_default(),
        );
        ungroup.execute(&mut project);
        let def = graph_of(&project, &fx);
        assert!(
            !def.nodes.iter().any(|n| n.group.is_some()),
            "no group nodes remain after ungroup"
        );
        assert!(
            def.nodes.iter().any(|n| n.handle.as_deref() == Some("b")),
            "b back at top level"
        );

        ungroup.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(
            def.nodes
                .iter()
                .any(|n| n.handle.as_deref() == Some("g") && n.group.is_some()),
            "undo of ungroup restores the group"
        );
    }

    /// Collapse `b` into a group, then confirm a scoped Move edit targets the
    /// body node (not a root node sharing its id) and undo restores it. This
    /// is the Layer 3.5 contract: editing inside a group descends to its level.
    #[test]
    fn scoped_move_targets_group_body() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        let mut mv = MoveGraphNodeCommand::new(
            GraphTarget::Effect(fx.clone()),
            1, // body node `b` kept its id when it moved into the group
            (42.0, 24.0),
            mirror_catalog_default(),
        )
        .with_scope(vec![g_id]);
        mv.execute(&mut project);

        let body_pos = |project: &Project| {
            graph_of(project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == g_id)
                .unwrap()
                .group
                .as_deref()
                .unwrap()
                .nodes
                .iter()
                .find(|n| n.handle.as_deref() == Some("b"))
                .unwrap()
                .editor_pos
        };
        assert_eq!(
            body_pos(&project),
            Some((42.0, 24.0)),
            "scoped move landed on the body node"
        );

        mv.undo(&mut project);
        assert_eq!(
            body_pos(&project),
            None,
            "undo restored the body node's editor_pos"
        );
    }

    /// A batch layout sets every listed node's `editor_pos` in one command,
    /// and undo restores them all — including the never-positioned `None`.
    #[test]
    fn layout_graph_nodes_sets_and_undoes_positions() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let pos_of = |project: &Project, id: u32| {
            graph_of(project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .editor_pos
        };
        assert_eq!(pos_of(&project, 0), None);
        assert_eq!(pos_of(&project, 2), None);

        let mut cmd = LayoutGraphNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![(0, (10.0, 20.0)), (2, (30.0, 40.0))],
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);
        assert_eq!(pos_of(&project, 0), Some((10.0, 20.0)));
        assert_eq!(pos_of(&project, 2), Some((30.0, 40.0)));

        cmd.undo(&mut project);
        assert_eq!(pos_of(&project, 0), None, "undo restored node 0");
        assert_eq!(pos_of(&project, 2), None, "undo restored node 2");
    }

    /// A scoped Add drops the new node into the group body, not the root, and
    /// undo removes it from the body.
    #[test]
    fn scoped_add_node_lands_in_group_body() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        let mut add = AddGraphNodeCommand::new(
            GraphTarget::Effect(fx.clone()),
            "node.transform".to_string(),
            Some((1.0, 2.0)),
            mirror_catalog_default(),
        )
        .with_scope(vec![g_id]);
        add.execute(&mut project);
        let new_id = add.new_node_id().expect("node added");

        let def = graph_of(&project, &fx);
        let body = def
            .nodes
            .iter()
            .find(|n| n.id == g_id)
            .unwrap()
            .group
            .as_deref()
            .unwrap();
        assert!(
            body.nodes.iter().any(|n| n.id == new_id),
            "new node added to the group body"
        );
        assert!(
            !def.nodes.iter().any(|n| n.id == new_id),
            "new node not added at root"
        );

        add.undo(&mut project);
        let def = graph_of(&project, &fx);
        let body = def
            .nodes
            .iter()
            .find(|n| n.id == g_id)
            .unwrap()
            .group
            .as_deref()
            .unwrap();
        assert!(
            !body.nodes.iter().any(|n| n.id == new_id),
            "undo removed the node from the body"
        );
    }

    /// Catalog default for a Mirror-like graph: source → uv_transform
    /// → mix → final_output, four nodes plus four wires. Mirrors the
    /// shape the runtime `build_mirror` produces.
    fn mirror_catalog_default() -> EffectGraphDef {
        let mut def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    node_id: manifold_core::NodeId::default(),
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 1,
                    node_id: manifold_core::NodeId::default(),
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 2,
                    node_id: manifold_core::NodeId::default(),
                    type_id: "node.mix".to_string(),
                    handle: Some("mix".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 3,
                    node_id: manifold_core::NodeId::default(),
                    type_id: "system.final_output".to_string(),
                    handle: Some("final_output".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
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
        };
        // Stamp node ids == handle, matching the bundled-preset convention
        // (a node's stable id is its authoring handle).
        for n in &mut def.nodes {
            if let Some(h) = n.handle.clone() {
                n.node_id = manifold_core::NodeId::new(h);
            }
        }
        def
    }

    /// Project with one master Mirror effect, graph: None.
    fn project_with_one_master_effect() -> (Project, EffectId) {
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
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
    fn remove_graph_node_prunes_bound_card_slider_and_undo_restores() {
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
        };
        use manifold_core::NodeId;

        let (mut project, id) = project_with_one_master_effect();
        // Diverged graph carrying a card slider bound to node 1 (uv_transform).
        let mut def = mirror_catalog_default();
        def.preset_metadata = Some(PresetMetadata {
            id: PresetTypeId::new("Mirror"),
            display_name: "Mirror".into(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".into(),
                name: "Amount".into(),
                min: 0.0,
                max: 1.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
            }],
            bindings: vec![BindingDef {
                id: "amount".into(),
                label: "Amount".into(),
                default_value: 0.5,
                target: BindingTarget::Node {
                    node_id: NodeId::new("uv_transform"),
                    param: "scale".into(),
                },
                convert: Default::default(),
                user_added: true,
                scale: 1.0,
                offset: 0.0,
            }],
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        {
            let fx = project.find_effect_by_id_mut(&id).unwrap();
            fx.graph = Some(def);
            fx.params = manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
        }

        let mut cmd = RemoveGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            1,
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(
            fx.graph.as_ref().unwrap().nodes.iter().all(|n| n.id != 1),
            "node deleted"
        );
        let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
        assert!(meta.bindings.is_empty(), "bound slider's binding pruned");
        assert!(meta.params.is_empty(), "bound slider's param spec pruned");
        assert!(fx.params.is_empty(), "its value slot pruned");

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(
            fx.graph.as_ref().unwrap().nodes.iter().any(|n| n.id == 1),
            "node restored"
        );
        let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
        assert_eq!(meta.bindings.len(), 1, "binding restored");
        assert_eq!(meta.params.len(), 1, "param spec restored");
        assert_eq!(fx.params.len(), 1, "value slot restored");
        let restored = fx.params.get("amount").unwrap();
        assert_eq!(restored.value, 0.5);
        assert_eq!(restored.base, 0.5);
        assert!(restored.exposed);
    }

    /// BUG-154: deleting a GROUP node that contains a node bound to a card
    /// slider used to leave the slider dangling — `remove_exposures_for_node`
    /// only ever matched the group container's own id, never the id of a
    /// node NESTED inside the removed group's subgraph. `subtree_node_ids`
    /// closes that: it walks the removed node's group tree and prunes an
    /// exposure bound to a node at ANY depth inside it.
    #[test]
    fn remove_group_node_prunes_card_slider_bound_to_a_nested_node() {
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
        };
        use manifold_core::NodeId;

        let (mut project, id) = project_with_one_master_effect();
        let mut def = mirror_catalog_default();
        // Rewrap node 1 ("uv_transform") as a group whose sole child carries
        // the SAME node_id — the slider stays bound to the nested node, not
        // the group container.
        let inner = def.nodes[1].clone();
        let group_node = EffectGraphNode {
            id: 1,
            node_id: NodeId::new("the_group"),
            type_id: GROUP_TYPE_ID.to_string(),
            handle: Some("the_group".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: vec![InterfacePortDef {
                        name: "source".to_string(),
                        port_type: String::new(),
                    }],
                    outputs: vec![InterfacePortDef {
                        name: "out".to_string(),
                        port_type: String::new(),
                    }],
                    params: vec![],
                },
                nodes: vec![inner],
                wires: vec![],
                tint: None,
            })),
        };
        def.nodes[1] = group_node;
        def.preset_metadata = Some(PresetMetadata {
            id: PresetTypeId::new("Mirror"),
            display_name: "Mirror".into(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".into(),
                name: "Amount".into(),
                min: 0.0,
                max: 1.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
            }],
            // Bound to the NESTED node's id ("uv_transform"), not the group
            // container's id ("the_group") — this is the exact configuration
            // BUG-154's cleanup used to miss.
            bindings: vec![BindingDef {
                id: "amount".into(),
                label: "Amount".into(),
                default_value: 0.5,
                target: BindingTarget::Node {
                    node_id: NodeId::new("uv_transform"),
                    param: "scale".into(),
                },
                convert: Default::default(),
                user_added: true,
                scale: 1.0,
                offset: 0.0,
            }],
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        {
            let fx = project.find_effect_by_id_mut(&id).unwrap();
            fx.graph = Some(def);
            fx.params = manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
        }

        let mut cmd = RemoveGraphNodeCommand::new(
            GraphTarget::Effect(id.clone()),
            1, // the group container
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(
            fx.graph.as_ref().unwrap().nodes.iter().all(|n| n.id != 1),
            "group node deleted"
        );
        let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
        assert!(
            meta.bindings.is_empty(),
            "slider bound to a node NESTED inside the removed group must be pruned"
        );
        assert!(meta.params.is_empty(), "its param spec pruned");
        assert!(fx.params.is_empty(), "its value slot pruned");

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&id).unwrap();
        let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
        assert_eq!(meta.bindings.len(), 1, "binding restored on undo");
        assert_eq!(fx.params.len(), 1, "value slot restored on undo");
    }

    #[test]
    fn revert_prunes_orphaned_automation_and_undo_restores() {
        let (mut project, id) = project_with_one_master_effect();
        {
            let fx = project.find_effect_by_id_mut(&id).unwrap();
            // A user-added binding (lives in the graph) with a driver hung on it.
            fx.append_user_binding(manifold_core::effects::UserParamBinding {
                id: "user.a.b.1".into(),
                label: "B".into(),
                node_id: manifold_core::NodeId::new("a"),
                legacy_node_handle: None,
                inner_param: "b".into(),
                min: 0.0,
                max: 1.0,
                default_value: 0.25,
                convert: manifold_core::effects::ParamConvert::Float,
                is_angle: false,
                invert: false,
                curve: Default::default(),
                scale: 1.0,
                offset: 0.0,
                value_labels: Vec::new(),
                section: None,
            });
            fx.create_driver(manifold_core::effects::ParamId::from("user.a.b.1"));
            assert!(fx.find_driver("user.a.b.1").is_some());
        }

        let mut cmd = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(fx.graph.is_none(), "graph reverted to catalog default");
        assert!(
            fx.find_driver("user.a.b.1").is_none(),
            "driver orphaned by the revert is pruned"
        );

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&id).unwrap();
        assert!(fx.graph.is_some(), "graph restored");
        assert!(
            fx.find_driver("user.a.b.1").is_some(),
            "the orphaned driver is re-attached on undo"
        );
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
    fn set_wgsl_source_sets_and_undo_restores_absence() {
        let (mut project, id) = project_with_one_master_effect();
        project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

        let mut cmd = SetWgslSourceCommand::new(
            GraphTarget::Effect(id.clone()),
            1,
            "fn main() {}".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(node.wgsl_source.as_deref(), Some("fn main() {}"));

        cmd.undo(&mut project);
        let def = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert!(node.wgsl_source.is_none(), "undo restores the absent source");
    }

    #[test]
    fn set_wgsl_source_empty_clears_back_to_builtin() {
        let (mut project, id) = project_with_one_master_effect();
        let mut def = mirror_catalog_default();
        // Pre-seed node 1 with a custom kernel so the clear has something to drop.
        def.nodes.iter_mut().find(|n| n.id == 1).unwrap().wgsl_source =
            Some("// custom".to_string());
        project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

        // An all-whitespace buffer clears the override rather than compiling empty.
        let mut cmd =
            SetWgslSourceCommand::new(GraphTarget::Effect(id.clone()), 1, "   ".to_string(), def);
        cmd.execute(&mut project);

        let after = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
        assert!(node.wgsl_source.is_none(), "blank source clears the override");

        cmd.undo(&mut project);
        let after = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
        let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(node.wgsl_source.as_deref(), Some("// custom"));
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

        // Serialize just the PresetInstance — what the project save
        // path emits per effect.
        let fx = project.find_effect_by_id(&id).unwrap();
        let json = serde_json::to_string(fx).unwrap();
        let back: manifold_core::effects::PresetInstance =
            serde_json::from_str(&json).unwrap();

        assert!(back.graph.is_some(), "graph field survived round-trip");
        let def = back.graph.as_ref().unwrap();
        assert_eq!(def.nodes.len(), 5, "appended Blur survived");
        assert!(def.nodes.iter().any(|n| n.type_id == "node.blur"));
    }

    // ─── Generator-target parity ────────────────────────────────────
    //
    // The same commands targeting `GraphTarget::Generator(layer_id)`
    // must mutate `Layer::generator_graph` rather than `PresetInstance::graph`.
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
            layer.generator_graph().is_some(),
            "generator_graph must lift from None on first edit",
        );
        let def = layer.generator_graph().unwrap();
        assert_eq!(def.nodes.len(), 5, "catalog 4 + new node = 5");
        assert!(def.nodes.iter().any(|n| n.type_id == "node.uv_field"));
        assert_eq!(layer.generator_graph_version(), 1);
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
            .gen_params_or_init().graph = Some(mirror_catalog_default());

        let mut revert = RevertEffectGraphCommand::new(GraphTarget::Generator(lid.clone()));
        revert.execute(&mut project);
        assert!(
            project
                .timeline
                .find_layer_by_id(&lid)
                .unwrap()
                .1
                .generator_graph()
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
                .generator_graph()
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
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
        );

        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
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
        let def = layer.generator_graph().unwrap();
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

    /// Regression (the on-node expose checkbox bug): exposing a param on a node
    /// *nested inside a group* — addressed the way the canvas actually addresses
    /// it, by `(scope_path, node_u32_id)` with an EMPTY stable `node_id` (bundled
    /// nodes ship empty) — must flip `exposed_params` on that nested node, NOT a
    /// top-level one. The old command scanned only the document root and matched
    /// by the empty `node_id`, so it hit the wrong node (or none): the checkbox
    /// never reflected the state and couldn't be unchecked. It must also mint the
    /// card binding with `node_id` defaulted to the handle — the same convention
    /// the runtime graph loader uses — so the slider actually drives the param.
    #[test]
    fn exposing_a_nested_node_param_targets_the_body_node_and_binds_by_handle() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        // Collapse `uv_transform` (doc id 1, empty stable node_id) into a group.
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        // Expose `rotation` exactly as the canvas would: empty stable node_id,
        // located by u32 doc id 1 at scope [g_id].
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
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
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        // The NESTED node carries the exposure.
        let body_has_rotation = |project: &Project| {
            graph_of(project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == g_id)
                .unwrap()
                .group
                .as_deref()
                .unwrap()
                .nodes
                .iter()
                .find(|n| n.handle.as_deref() == Some("uv_transform"))
                .unwrap()
                .exposed_params
                .contains("rotation")
        };
        assert!(
            body_has_rotation(&project),
            "expose flipped the nested body node's exposed_params"
        );
        // No ROOT node absorbed it (the old empty-node_id top-level scan bug).
        assert!(
            graph_of(&project, &fx)
                .nodes
                .iter()
                .all(|n| !n.exposed_params.contains("rotation")),
            "no top-level node was wrongly exposed"
        );

        // The card binding targets the handle-defaulted id, so it resolves to
        // the runtime node (`graph_loader` applies the same default) — not a
        // dead empty-id binding.
        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1, "one user binding minted");
        assert_eq!(
            ub[0].node_id, "uv_transform",
            "binding node_id defaults to the handle"
        );

        // Undo clears the nested exposure.
        expose.undo(&mut project);
        assert!(
            !body_has_rotation(&project),
            "undo restored the nested node's exposed_params"
        );
    }

    // ─── D5 card sections (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2) ────

    #[test]
    fn exposing_inside_a_group_stamps_section_from_the_group_name() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
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
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1);
        let entry = fx_inst.params.get(&ub[0].id).expect("manifest entry for the new binding");
        assert_eq!(
            entry.spec.section.as_deref(),
            Some("g"),
            "expose-time seeding stamps the innermost enclosing group's display name"
        );

        // Undo removes the whole binding (spec + section together) — no
        // dangling manifest entry.
        expose.undo(&mut project);
        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        assert!(fx_inst.params.get(&ub[0].id).is_none(), "undo removed the manifest entry entirely");
    }

    #[test]
    fn exposing_at_top_level_leaves_section_none() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        // No grouping — expose `rotation` directly at the document root
        // (empty scope_path).
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
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
            Vec::new(),
        );
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1);
        let entry = fx_inst.params.get(&ub[0].id).unwrap();
        assert_eq!(entry.spec.section, None, "a top-level expose gets no section");
    }

    #[test]
    fn exposing_survives_json_round_trip_with_section() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
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
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub_id = fx_inst.user_param_bindings()[0].id.clone();

        // "save" — serialize the instance (this is what the project save
        // path emits per effect; PARAM_STORAGE_BOUNDARIES_DESIGN D12 derives
        // `graph.preset_metadata.params` from the live manifest here).
        let json = serde_json::to_string(fx_inst).unwrap();
        // "reload"
        let back: manifold_core::effects::PresetInstance = serde_json::from_str(&json).unwrap();
        let spec = back
            .graph
            .as_ref()
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|p| p.id == ub_id)
            .expect("the exposed param's spec survived the round trip");
        assert_eq!(
            spec.section.as_deref(),
            Some("g"),
            "the card row is still sectioned after save -> reload"
        );
    }

    #[test]
    fn exposing_a_non_preset_param_on_generator_appends_user_binding_and_grows_param_values() {
        // Regression: clicking the expose checkbox on a generator's
        // inner-node param that has NO preset binding (e.g.
        // `node.draw_lines:animate` on the Wireframe preset) must
        // synthesize a user-added BindingDef + ParamSpecDef in the
        // graph's preset_metadata AND extend gp.param_values by one
        // slot so the outer card has somewhere to render it.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, ParamSpecDef,
            PresetMetadata,
        };
        use manifold_core::effects::ParamConvert;
        use manifold_core::preset_type_id::PresetTypeId;

        // Wireframe-like preset: two bundled bindings ("shape" → render.shape,
        // "scale" → render.scale) plus an inner node `render` whose
        // `animate` param is NOT bound. param_values has two bundled
        // slots.
        let preset_def = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("test.wireframe"),
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
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
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
                        curve: Default::default(),
                        is_angle: false,
                        invert: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                    BindingDef {
                        id: "scale".into(),
                        label: "Scale".into(),
                        default_value: 1.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "scale".into(),
                        },
                        convert: ParamConvert::Float,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("render"),
                type_id: "node.draw_lines".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.gen_params_or_init().graph = Some(preset_def());
            // gen_params starts with the two bundled slot values.
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(PresetTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            // Override values after init — the registry doesn't know
            // about our synthetic preset, so init may leave the vec
            // empty. Force the bundled slot count to match the preset.
            gp.params = manifold_core::params::ParamManifest::from_params(vec![
                slot("shape", 0.0, true),
                slot("scale", 1.0, true),
            ]);
            // slot() seeds base = value; mark base tracked (fork #16).
            gp.base_tracked = true;
        }

        // Expose `render.animate` — has no preset binding, so the
        // command must synthesize a user-added entry.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("render"),
            0,
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
            Vec::new(),
        );
        expose.execute(&mut project);

        // Assert: preset_metadata grew by one entry in both lists,
        // marked user_added=true.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
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
                BindingTarget::Node { node_id, param }
                    if node_id == "render" && param == "animate"
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

        // gp.params grew by one to match.
        let gp = layer.gen_params().unwrap();
        assert_eq!(
            gp.params.len(),
            3,
            "params grew by one slot for the user-added binding"
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
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo removes the user-added param");
        assert_eq!(
            meta.bindings.len(),
            2,
            "undo removes the user-added binding"
        );
        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 2, "undo pops the user-added slot");
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
        let def = layer.generator_graph().unwrap();
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
        use manifold_core::preset_type_id::PresetTypeId;
        use manifold_core::types::{BeatDivision, DriverWaveform};

        // Preset already carries a user-added binding (simulates
        // "user-added in a prior session, now loaded from a save
        // file"). One bundled binding + one user-added binding.
        let preset_def_with_user_added = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("test.wireframe"),
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
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
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
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                    BindingDef {
                        id: "user.render.animate.1".into(),
                        label: "Animate".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "animate".into(),
                        },
                        convert: ParamConvert::BoolThreshold,
                        user_added: true,
                        scale: 1.0,
                        offset: 0.0,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("render"),
                type_id: "node.draw_lines".to_string(),
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
                group: None,
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.gen_params_or_init().graph = Some(preset_def_with_user_added());
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(PresetTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            gp.params = manifold_core::params::ParamManifest::from_params(vec![
                slot("shape", 0.0, true),
                slot("user.render.animate.1", 0.75, true),
            ]); // bundled `shape` + user-added `animate`
            gp.base_tracked = true;
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
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            gp.envelopes = Some(vec![ParamEnvelope::new(std::borrow::Cow::Owned(
                "user.render.animate.1".to_string(),
            ))]);
        }

        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("render"),
            0,
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
            Vec::new(),
        );
        unexpose.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 1, "user-added param removed");
        assert_eq!(meta.bindings.len(), 1, "user-added binding removed");
        assert_eq!(meta.bindings[0].id, "shape", "bundled binding survives");

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 1, "user-added slot removed");
        assert_eq!(
            gp.params.get("shape").unwrap().value,
            0.0,
            "bundled `shape` value intact"
        );
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
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo restores user-added param");
        assert_eq!(meta.bindings.len(), 2, "undo restores user-added binding");
        assert_eq!(meta.bindings[1].id, "user.render.animate.1");
        assert!(meta.bindings[1].user_added);

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 2, "undo restores the slot");
        assert!(
            (gp.params.get("user.render.animate.1").unwrap().value - 0.75).abs() < f32::EPSILON,
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
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        // Expose first.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
        );
        expose.execute(&mut project);

        // Now attach a driver + ableton mapping to the synthesised
        // user_param_id.
        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            let ub = fx.user_param_bindings();
            assert_eq!(ub.len(), 1);
            ub[0].id.clone()
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
                free_period_beats: None,
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
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
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
        assert_eq!(fx.user_param_bindings().len(), 1, "binding restored");
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
        // Same shape as the driver/Ableton orphan-cleanup test, for
        // envelopes — which since envelope-home unification live on the
        // effect instance. Unexpose prunes envelopes matching the
        // binding's param_id (in the same borrow as drivers/Ableton) and
        // restores them on undo.
        use manifold_core::effects::{ParamConvert, ParamEnvelope};
        use manifold_core::layer::Layer;
        use manifold_core::types::LayerType;

        let effect_type = PresetTypeId::new("test.mirror");
        let effect_id = EffectId::new("envelope-cleanup-test");

        let mut project = Project::default();
        let mut layer = Layer::new("Test".to_string(), LayerType::Generator, 0);
        let mut fx = PresetInstance::new(effect_type.clone());
        fx.id = effect_id.clone();
        layer.effects = Some(vec![fx]);
        project.timeline.layers.push(layer);

        // Expose first, attach an envelope to the synthesised id.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
        );
        expose.execute(&mut project);

        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            fx.user_param_bindings()[0].id.clone()
        };
        {
            let fx = project.find_effect_by_id_mut(&effect_id).unwrap();
            fx.envelopes_mut().push(ParamEnvelope::new(user_param_id.clone()));
            // Add an unrelated envelope that should NOT get pruned —
            // different param_id.
            fx.envelopes_mut().push(ParamEnvelope::new("unrelated.param".to_string()));
        }

        // Unexpose. The matching envelope must be pruned; the unrelated
        // one must survive.
        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
        );
        unexpose.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let envs = fx.envelopes.as_deref().unwrap_or(&[]);
        assert_eq!(envs.len(), 1, "matching envelope pruned, unrelated kept");
        assert_eq!(envs[0].param_id, "unrelated.param");

        // Undo restores the pruned envelope alongside the binding.
        unexpose.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let envs = fx.envelopes.as_deref().unwrap_or(&[]);
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
                id: PresetTypeId::new("test.plasma"),
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
                    curve: Default::default(),
                    invert: false,
                    is_angle: false,
                    is_trigger_gate: false,
                    wraps: false,
                    section: None,
                }],
                bindings: vec![BindingDef {
                    id: "pattern".into(),
                    label: "Pattern".into(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: manifold_core::NodeId::new("gen"),
                        param: "pattern".into(),
                    },
                    convert: ParamConvert::EnumRound,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("gen"),
                type_id: "node.plasma_pattern_2d".to_string(),
                handle: Some("gen".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
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
            .gen_params_or_init().graph = Some(preset_def_with_pattern_binding());

        // UNCHECK Pattern.
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("gen"),
            0,
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
            Vec::new(),
        );
        cmd.execute(&mut project);

        // The def must NOT contain "pattern" in exposed_params for
        // the "gen" node.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
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
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
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
        // Effect-side mirror: a user-added binding was appended to the
        // graph metadata because the catalog default has no preset
        // bindings for this param.
        let ub = fx.user_param_bindings();
        assert_eq!(ub.len(), 1);
        assert_eq!(ub[0].node_id, "uv_transform");
        assert_eq!(ub[0].inner_param, "rotation");

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
        assert_eq!(fx.user_param_bindings().len(), 0);
    }

    /// `PARAM_TWO_WAY_BINDING_DESIGN.md` D9: unmapping a user-added binding
    /// freezes the card's current effective value into the def slot the
    /// binding stops governing, instead of leaving whatever stale value sat
    /// there — so the render never visually snaps on unmap.
    #[test]
    fn unexpose_user_binding_freezes_effective_value_into_def_slot() {
        let mut project = Project::default();
        let effect_id = EffectId::new("test-mirror-instance-freeze");
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        // Expose rotation (appends a user binding).
        let mut expose_cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
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
            Vec::new(),
        );
        expose_cmd.execute(&mut project);

        // Move the card away from default, as a performer would — through
        // the same command the card's own slider drag commits via
        // (`ChangeGraphParamCommand`, `commands/effects.rs`), not a raw
        // manifest poke.
        let binding_id = project
            .find_effect_by_id(&effect_id)
            .unwrap()
            .user_param_bindings()[0]
            .id
            .clone();
        let mut set_cmd = crate::commands::effects::ChangeGraphParamCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            binding_id,
            0.0,
            77.0,
        );
        set_cmd.execute(&mut project);

        // Unexpose — this removes the user binding and must freeze 77.0
        // into the def's `rotation` slot before the binding goes away.
        let mut unexpose_cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );
        unexpose_cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.user_param_bindings().len(), 0, "binding removed");
        let def = fx.graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        match node.params.get("rotation") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 77.0).abs() < 1e-6, "expected frozen 77.0, got {value}");
            }
            other => panic!("expected a frozen Float value, got {other:?}"),
        }
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
        let def = layer.generator_graph().unwrap();
        let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
        let v = node.params.get("rotation").unwrap();
        match v {
            SerializedParamValue::Float { value } => assert!((value - 45.0).abs() < 1e-6),
            _ => panic!("expected Float param value"),
        }
    }

    // ── group tint + rename ──

    /// Collapse node 1 into a group `g` and return the project + the group's id.
    fn project_with_group(handle: &str) -> (Project, EffectId, u32) {
        let (mut project, fx) = project_with_graph(abc_graph());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            handle.to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let gid = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some(handle))
            .unwrap()
            .id;
        (project, fx, gid)
    }

    #[test]
    fn set_group_tint_applies_and_undo_restores() {
        let (mut project, fx, gid) = project_with_group("g");
        let mut cmd = SetGroupTintCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            gid,
            Some([0.4, 0.2, 0.4, 1.0]),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);
        let g = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.id == gid)
            .unwrap();
        assert_eq!(g.group.as_ref().unwrap().tint, Some([0.4, 0.2, 0.4, 1.0]));

        cmd.undo(&mut project);
        let g = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.id == gid)
            .unwrap();
        assert_eq!(g.group.as_ref().unwrap().tint, None, "tint restored to default");
    }

    #[test]
    fn rename_group_applies_undo_restores_and_rejects_invalid() {
        let (mut project, fx, gid) = project_with_group("g");

        // Valid rename.
        let mut rn = RenameGroupCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            gid,
            "fx_chain".to_string(),
            mirror_catalog_default(),
        );
        rn.execute(&mut project);
        assert_eq!(
            graph_of(&project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == gid)
                .unwrap()
                .handle
                .as_deref(),
            Some("fx_chain")
        );
        rn.undo(&mut project);
        assert_eq!(
            graph_of(&project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == gid)
                .unwrap()
                .handle
                .as_deref(),
            Some("g"),
            "handle restored on undo"
        );

        // A `/`-bearing name is rejected (the handle is a namespace) — no-op.
        let mut bad = RenameGroupCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            gid,
            "a/b".to_string(),
            mirror_catalog_default(),
        );
        bad.execute(&mut project);
        assert_eq!(
            graph_of(&project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == gid)
                .unwrap()
                .handle
                .as_deref(),
            Some("g"),
            "invalid name left the group unchanged"
        );
    }

    /// D5 rename-sweep setup: `project_with_group("g")` (node "b" grouped
    /// under "g") plus an exposed param on "b", scoped inside the group so
    /// its section seeds to `Some("g")`. Returns `(project, fx, gid,
    /// user_param_id)`.
    fn project_with_group_and_sectioned_param() -> (Project, EffectId, u32, String) {
        let (mut project, fx, gid) = project_with_group("g");
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
            "b".to_string(),
            "amount".to_string(),
            true,
            mirror_catalog_default(),
            "Amount".to_string(),
            0.0,
            1.0,
            0.5,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        )
        .with_scope(vec![gid]);
        expose.execute(&mut project);
        let ub_id = project
            .find_effect_by_id(&fx)
            .unwrap()
            .user_param_bindings()[0]
            .id
            .clone();
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("g"),
            "setup: expose seeded the section from the group name"
        );
        (project, fx, gid, ub_id)
    }

    #[test]
    fn rename_group_sweeps_matching_sections_and_undo_restores() {
        let (mut project, fx, gid, ub_id) = project_with_group_and_sectioned_param();

        let mut rn = RenameGroupCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            gid,
            "leaf".to_string(),
            mirror_catalog_default(),
        );
        rn.execute(&mut project);
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("leaf"),
            "section follows the rename"
        );

        rn.undo(&mut project);
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("g"),
            "undo restores the pre-rename section"
        );
    }

    #[test]
    fn rename_group_leaves_hand_edited_section_untouched() {
        let (mut project, fx, gid, ub_id) = project_with_group_and_sectioned_param();

        // Hand-edit the section via the mapping editor to something that no
        // longer matches the group's current name.
        project
            .find_effect_by_id_mut(&fx)
            .unwrap()
            .params
            .get_mut(&ub_id)
            .unwrap()
            .spec
            .section = Some("Custom".to_string());

        let mut rn = RenameGroupCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            gid,
            "leaf2".to_string(),
            mirror_catalog_default(),
        );
        rn.execute(&mut project);
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("Custom"),
            "a hand-edited section (no longer matching the old group name) survives the rename sweep"
        );
    }

    // ── paste / duplicate ──

    #[test]
    fn paste_node_clones_with_fresh_identity_and_undo_removes() {
        let (mut project, fx) = project_with_graph(abc_graph());
        let src = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("b"))
            .unwrap()
            .clone();
        let before = graph_of(&project, &fx).nodes.len();

        let mut cmd = PasteNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![src.clone()],
            vec![],
            (30.0, 30.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        assert_eq!(def.nodes.len(), before + 1);
        let copy = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("b_2"))
            .expect("handle deduped to b_2");
        assert_ne!(copy.id, src.id, "fresh runtime id");
        assert_ne!(copy.node_id, src.node_id, "fresh stable node_id");
        assert_eq!(copy.type_id, src.type_id, "same node type");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def.nodes.len(), before);
        assert!(!def.nodes.iter().any(|n| n.handle.as_deref() == Some("b_2")));
    }

    #[test]
    fn paste_remaps_internal_wires_to_the_new_node_ids() {
        let (mut project, fx) = project_with_graph(abc_graph());
        // Copy a (0) and b (1) plus the internal wire a -> b.
        let def = graph_of(&project, &fx);
        let a = def.nodes.iter().find(|n| n.id == 0).unwrap().clone();
        let b = def.nodes.iter().find(|n| n.id == 1).unwrap().clone();
        let wire_ab = def
            .wires
            .iter()
            .find(|w| w.from_node == 0 && w.to_node == 1)
            .unwrap()
            .clone();
        let wires_before = def.wires.len();

        let mut cmd = PasteNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![a, b],
            vec![wire_ab],
            (30.0, 30.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let a2 = def.nodes.iter().find(|n| n.handle.as_deref() == Some("a_2")).unwrap();
        let b2 = def.nodes.iter().find(|n| n.handle.as_deref() == Some("b_2")).unwrap();
        assert_eq!(def.wires.len(), wires_before + 1, "one internal wire pasted");
        assert!(
            def.wires
                .iter()
                .any(|w| w.from_node == a2.id && w.to_node == b2.id),
            "the copied wire re-anchored to the new node ids"
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def.wires.len(), wires_before, "pasted wire removed on undo");
    }

    // ── scene build P5: add-object / add-light gestures ──

    /// A single `node.render_scene` node (id 0) with `objects`/`lights` set to
    /// the given counts — the fixture `AddSceneObjectCommand`/
    /// `AddSceneLightCommand` operate against.
    fn render_scene_graph(objects: u32, lights: u32) -> EffectGraphDef {
        let mut render = EffectGraphNode {
            id: 0,
            node_id: manifold_core::NodeId::new("render"),
            type_id: "node.render_scene".to_string(),
            handle: Some("render".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        render
            .params
            .insert("objects".to_string(), SerializedParamValue::Float { value: objects as f32 });
        render
            .params
            .insert("lights".to_string(), SerializedParamValue::Float { value: lights as f32 });
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render],
            wires: vec![],
        }
    }

    #[test]
    fn add_scene_object_command_bumps_count_builds_group_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = AddSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            2, // next_index — matches the fixture's current `objects` (2)
            (100.0, 200.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("objects"),
            Some(&SerializedParamValue::Float { value: 3.0 }),
            "objects bumped by one"
        );

        let group = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("Object 3"))
            .expect("named group created");
        assert_eq!(group.editor_pos, Some((100.0, 200.0)));
        let body = group.group.as_deref().expect("is a group node");
        assert_eq!(body.nodes.len(), 4, "cube + material + transform + group_output boundary");
        assert!(body.nodes.iter().any(|n| n.type_id == "node.cube_mesh"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.phong_material"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.transform_3d"));
        assert_eq!(body.wires.len(), 3, "mesh/material/transform each wired to the group_output");
        assert_eq!(body.interface.outputs.len(), 3);

        // The group's three outputs wired to the new object-2 slot's ports.
        assert!(def.wires.iter().any(|w| w.from_node == group.id
            && w.from_port == "vertices"
            && w.to_node == 0
            && w.to_port == "mesh_2"));
        assert!(def.wires.iter().any(|w| w.from_node == group.id
            && w.from_port == "material"
            && w.to_node == 0
            && w.to_port == "material_2"));
        assert!(def.wires.iter().any(|w| w.from_node == group.id
            && w.from_port == "transform"
            && w.to_node == 0
            && w.to_port == "transform_2"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }

    #[test]
    fn add_scene_light_command_bumps_count_wires_bare_light_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = AddSceneLightCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            1, // next_index — matches the fixture's current `lights` (1)
            (-260.0, 50.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("lights"),
            Some(&SerializedParamValue::Float { value: 2.0 }),
            "lights bumped by one"
        );

        let light = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("light_1"))
            .expect("bare light node created");
        assert!(light.group.is_none(), "D7a: no group around the light");
        assert_eq!(light.type_id, "node.light");
        assert_eq!(light.editor_pos, Some((-260.0, 50.0)));

        // D7a defaults, transcribed.
        assert_eq!(light.params.get("mode"), Some(&SerializedParamValue::Enum { value: 0 }));
        assert_eq!(light.params.get("color_r"), Some(&SerializedParamValue::Float { value: 1.0 }));
        assert_eq!(light.params.get("color_g"), Some(&SerializedParamValue::Float { value: 1.0 }));
        assert_eq!(light.params.get("color_b"), Some(&SerializedParamValue::Float { value: 1.0 }));
        assert_eq!(light.params.get("intensity"), Some(&SerializedParamValue::Float { value: 1.0 }));
        assert_eq!(light.params.get("cast_shadows"), Some(&SerializedParamValue::Float { value: 1.0 }));

        // Auto-wired into the new light_1 slot — "add means added," never a
        // bumped count with a dead port.
        assert!(def
            .wires
            .iter()
            .any(|w| w.from_node == light.id && w.from_port == "out" && w.to_node == 0 && w.to_port == "light_1"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }

    // ── SCENE_SETUP_PANEL_DESIGN P1: Add Environment / Add Fog ──

    #[test]
    fn add_scene_environment_command_spawns_bake_environment_and_wires_envmap() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = AddSceneEnvironmentCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            (10.0, 20.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let env = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.bake_environment")
            .expect("environment node created");
        assert_eq!(env.editor_pos, Some((10.0, 20.0)));
        assert_eq!(env.params.get("intensity"), Some(&SerializedParamValue::Float { value: 1.0 }));
        assert!(def
            .wires
            .iter()
            .any(|w| w.from_node == env.id && w.from_port == "envmap" && w.to_node == 0 && w.to_port == "envmap"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }

    #[test]
    fn add_scene_fog_command_spawns_atmosphere_and_wires_atmosphere_port() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = AddSceneFogCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            (30.0, 40.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let fog = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.atmosphere")
            .expect("fog node created");
        assert_eq!(fog.editor_pos, Some((30.0, 40.0)));
        assert!(def.wires.iter().any(|w| w.from_node == fog.id
            && w.from_port == "atmosphere"
            && w.to_node == 0
            && w.to_port == "atmosphere"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }
}
