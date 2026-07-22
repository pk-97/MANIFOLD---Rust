//! Node-edit graph commands — add/remove nodes, connect/disconnect ports,
//! move & layout, set node param, set WGSL source, revert. Split out of
//! `graph.rs` in P2-G/S2 (pure move); the target-graph access helpers and
//! `descend_level` stay in `graph/mod.rs` and are used here via `super`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::project::Project;

use crate::command::Command;

use super::{
    descend_level, install_target_graph, take_target_graph, with_existing_target_graph_mut,
    with_target_graph_mut,
};

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

    /// Seed `previous_value` explicitly instead of letting `execute()`
    /// self-capture it off whatever's in the graph at execute time.
    ///
    /// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D4): self-capture is
    /// correct only when `execute()` runs exactly once, before any other
    /// write has touched the same key — true for every existing call site
    /// (one edit, one command). It is WRONG for a drag-cadence commit built
    /// from a live-preview gesture: by the time the ONE commit command's
    /// `execute()` actually runs (locally, or later on the content thread
    /// once queued `MutateProjectLive` motion writes have already applied),
    /// the graph already holds the POST-drag value, so self-capture would
    /// record `previous_value == new_value` — an undo that restores
    /// nothing. The caller already holds the true pre-drag value (captured
    /// at `ParamSnapshot`, before any write); this lets it hand that value
    /// to the command directly, so `execute()`'s self-capture guard
    /// (`prev_already_captured`) skips and `undo()` restores the real
    /// pre-drag state. `None` means "the key was absent before the drag."
    pub fn with_previous(mut self, previous: Option<SerializedParamValue>) -> Self {
        self.previous_value = Some(previous);
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

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::test_support::*;
    use manifold_core::EffectId;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef};
    use manifold_core::effects::PresetInstance;
    use crate::command::Command;

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
                card_visible: true,
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
                card_visible: true,
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
}
