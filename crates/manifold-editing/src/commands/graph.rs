//! Graph mutation commands — Phase 3 of per-card divergence.
//!
//! Each command operates on an [`EffectInstance`](
//! manifold_core::effects::EffectInstance)'s per-card graph topology
//! ([`EffectInstance::graph`]). They lift a `None` graph to a clone of
//! the supplied catalog default on first edit, apply the mutation,
//! and bump [`EffectInstance::graph_version`] so the renderer can
//! detect the change.
//!
//! All commands key on stable [`EffectId`] rather than `target +
//! index` — the editor canvas is always open on a specific instance,
//! and the id is reorder-stable. Each command stores reverse state for
//! undo/redo.
//!
//! Phase 3 of the per-card-divergence plan in
//! `docs/NODE_GRAPH_SYSTEM.md`.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::id::EffectId;
use manifold_core::project::Project;

use crate::command::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Lift a `None` graph to a clone of `catalog_default`, returning a
/// mutable reference to the def. If the graph is already `Some`,
/// returns a reference to the existing one. Common prelude for every
/// graph-mutation command.
fn ensure_graph<'a>(
    instance: &'a mut manifold_core::effects::EffectInstance,
    catalog_default: &EffectGraphDef,
) -> &'a mut EffectGraphDef {
    instance.graph.get_or_insert_with(|| catalog_default.clone())
}

/// Bump `graph_version` to signal the renderer that the topology has
/// changed. Wraps on overflow so a long session doesn't panic.
fn bump_version(instance: &mut manifold_core::effects::EffectInstance) {
    instance.graph_version = instance.graph_version.wrapping_add(1);
}

// ---------------------------------------------------------------------------
// Add Graph Node
// ---------------------------------------------------------------------------

/// Add a new node to the per-card graph at the given editor position.
/// The new node has default parameters and no port wires until a
/// subsequent [`ConnectPortsCommand`] connects it.
#[derive(Debug)]
pub struct AddGraphNodeCommand {
    effect_id: EffectId,
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
        effect_id: EffectId,
        node_type_id: String,
        pos: Option<(f32, f32)>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            effect_id,
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
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            eprintln!(
                "[manifold-editing] AddGraphNode: effect id {:?} not found",
                self.effect_id
            );
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        let next_id = def
            .nodes
            .iter()
            .map(|n| n.id)
            .max()
            .map_or(0, |m| m + 1);
        let id = self.minted_id.unwrap_or(next_id);
        def.nodes.push(EffectGraphNode {
            id,
            type_id: self.node_type_id.clone(),
            handle: None,
            params: BTreeMap::new(),
            editor_pos: self.pos,
        });
        self.minted_id = Some(id);
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(id) = self.minted_id else { return };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        if let Some(def) = instance.graph.as_mut() {
            def.nodes.retain(|n| n.id != id);
            def.wires.retain(|w| w.from_node != id && w.to_node != id);
        }
        bump_version(instance);
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
    effect_id: EffectId,
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
    pub fn new(effect_id: EffectId, node_id: u32, catalog_default: EffectGraphDef) -> Self {
        Self {
            effect_id,
            node_id,
            catalog_default,
            removed: None,
        }
    }
}

impl Command for RemoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        let Some(node_pos) = def.nodes.iter().position(|n| n.id == self.node_id) else {
            // Idempotent: node already gone.
            return;
        };
        let node = def.nodes.remove(node_pos);
        let removed_wires: Vec<EffectGraphWire> = def
            .wires
            .iter()
            .filter(|w| w.from_node == self.node_id || w.to_node == self.node_id)
            .cloned()
            .collect();
        def.wires
            .retain(|w| w.from_node != self.node_id && w.to_node != self.node_id);
        self.removed = Some(RemovedNode {
            node,
            wires: removed_wires,
        });
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(removed) = self.removed.clone() else {
            return;
        };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let Some(def) = instance.graph.as_mut() else {
            return;
        };
        def.nodes.push(removed.node);
        def.wires.extend(removed.wires);
        bump_version(instance);
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
    effect_id: EffectId,
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
        effect_id: EffectId,
        from_node: u32,
        from_port: String,
        to_node: u32,
        to_port: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            effect_id,
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
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        // Displace any existing wire to the same input port.
        if let Some(existing) = def
            .wires
            .iter()
            .position(|w| w.to_node == self.to_node && w.to_port == self.to_port)
        {
            self.displaced = Some(def.wires.remove(existing));
        }
        def.wires.push(EffectGraphWire {
            from_node: self.from_node,
            from_port: self.from_port.clone(),
            to_node: self.to_node,
            to_port: self.to_port.clone(),
        });
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let Some(def) = instance.graph.as_mut() else {
            return;
        };
        // Remove the wire we added.
        if let Some(pos) = def.wires.iter().position(|w| {
            w.from_node == self.from_node
                && w.from_port == self.from_port
                && w.to_node == self.to_node
                && w.to_port == self.to_port
        }) {
            def.wires.remove(pos);
        }
        // Restore the displaced wire if there was one.
        if let Some(wire) = self.displaced.take() {
            def.wires.push(wire);
        }
        bump_version(instance);
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
    effect_id: EffectId,
    to_node: u32,
    to_port: String,
    catalog_default: EffectGraphDef,
    /// The wire we removed, restored by undo.
    removed: Option<EffectGraphWire>,
}

impl DisconnectPortsCommand {
    pub fn new(
        effect_id: EffectId,
        to_node: u32,
        to_port: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            effect_id,
            to_node,
            to_port,
            catalog_default,
            removed: None,
        }
    }
}

impl Command for DisconnectPortsCommand {
    fn execute(&mut self, project: &mut Project) {
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        if let Some(pos) = def
            .wires
            .iter()
            .position(|w| w.to_node == self.to_node && w.to_port == self.to_port)
        {
            self.removed = Some(def.wires.remove(pos));
            bump_version(instance);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(wire) = self.removed.take() else {
            return;
        };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let Some(def) = instance.graph.as_mut() else {
            return;
        };
        def.wires.push(wire);
        bump_version(instance);
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
    effect_id: EffectId,
    node_id: u32,
    new_pos: (f32, f32),
    catalog_default: EffectGraphDef,
    /// Position before execute(), for undo.
    previous_pos: Option<Option<(f32, f32)>>,
}

impl MoveGraphNodeCommand {
    pub fn new(
        effect_id: EffectId,
        node_id: u32,
        new_pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            effect_id,
            node_id,
            new_pos,
            catalog_default,
            previous_pos: None,
        }
    }
}

impl Command for MoveGraphNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        let Some(node) = def.nodes.iter_mut().find(|n| n.id == self.node_id) else {
            return;
        };
        if self.previous_pos.is_none() {
            self.previous_pos = Some(node.editor_pos);
        }
        node.editor_pos = Some(self.new_pos);
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous) = self.previous_pos else {
            return;
        };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let Some(def) = instance.graph.as_mut() else {
            return;
        };
        if let Some(node) = def.nodes.iter_mut().find(|n| n.id == self.node_id) {
            node.editor_pos = previous;
        }
        bump_version(instance);
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
    effect_id: EffectId,
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
        effect_id: EffectId,
        node_id: u32,
        param_name: String,
        new_value: SerializedParamValue,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            effect_id,
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
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let def = ensure_graph(instance, &self.catalog_default);
        let Some(node) = def.nodes.iter_mut().find(|n| n.id == self.node_id) else {
            return;
        };
        let prev = node.params.insert(self.param_name.clone(), self.new_value);
        if self.previous_value.is_none() {
            self.previous_value = Some(prev);
        }
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous_value.take() else {
            return;
        };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        let Some(def) = instance.graph.as_mut() else {
            return;
        };
        let Some(node) = def.nodes.iter_mut().find(|n| n.id == self.node_id) else {
            return;
        };
        match prev {
            Some(v) => {
                node.params.insert(self.param_name.clone(), v);
            }
            None => {
                node.params.remove(&self.param_name);
            }
        }
        bump_version(instance);
    }

    fn description(&self) -> &str {
        "Set Graph Node Param"
    }
}

// ---------------------------------------------------------------------------
// Revert Effect Graph
// ---------------------------------------------------------------------------

/// Clear an [`EffectInstance`](manifold_core::effects::EffectInstance)'s
/// per-card graph override, reverting the effect to the bundled
/// preset. The next chain rebuild reads the catalog default instead of
/// the saved-in-place graph.
///
/// Idempotent on execute: if `instance.graph` is already `None`, the
/// command stores `None` for undo and does nothing else. On undo,
/// restores the previous `Some(def)` if there was one.
///
/// The "library picker" in §6.6 #30 surfaces this command as the user-
/// facing "Reset to Default Preset" action on a diverged effect.
#[derive(Debug)]
pub struct RevertEffectGraphCommand {
    effect_id: EffectId,
    /// Pre-execute snapshot of `instance.graph`. `None` if the effect
    /// was already on the catalog default, `Some(def)` if it had a
    /// per-card override that this command cleared.
    previous: Option<Option<EffectGraphDef>>,
}

impl RevertEffectGraphCommand {
    pub fn new(effect_id: EffectId) -> Self {
        Self {
            effect_id,
            previous: None,
        }
    }
}

impl Command for RevertEffectGraphCommand {
    fn execute(&mut self, project: &mut Project) {
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        if self.previous.is_none() {
            self.previous = Some(instance.graph.take());
        } else {
            instance.graph = None;
        }
        bump_version(instance);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.previous.take() else {
            return;
        };
        let Some(instance) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        instance.graph = prev;
        bump_version(instance);
    }

    fn description(&self) -> &str {
        "Revert Effect Graph"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
    use manifold_core::effects::EffectInstance;
    use manifold_core::EffectTypeId;

    /// Catalog default for a Mirror-like graph: source → uv_transform
    /// → mix → final_output, four nodes plus four wires. Mirrors the
    /// shape the runtime `build_mirror` produces.
    fn mirror_catalog_default() -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: BTreeMap::new(),
                    editor_pos: None,
                },
                EffectGraphNode {
                    id: 1,
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params: BTreeMap::new(),
                    editor_pos: None,
                },
                EffectGraphNode {
                    id: 2,
                    type_id: "node.mix".to_string(),
                    handle: Some("mix".to_string()),
                    params: BTreeMap::new(),
                    editor_pos: None,
                },
                EffectGraphNode {
                    id: 3,
                    type_id: "system.final_output".to_string(),
                    handle: Some("final_output".to_string()),
                    params: BTreeMap::new(),
                    editor_pos: None,
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
            id.clone(),
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
            id.clone(),
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
            RemoveGraphNodeCommand::new(id.clone(), 1, mirror_catalog_default());
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
            RemoveGraphNodeCommand::new(id.clone(), 1, mirror_catalog_default());
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
            id.clone(),
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
            id.clone(),
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
            MoveGraphNodeCommand::new(id.clone(), 1, (100.0, 200.0), mirror_catalog_default());
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
            id.clone(),
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
            id.clone(),
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
            id.clone(),
            "node.blur".to_string(),
            None,
            mirror_catalog_default(),
        );
        add.execute(&mut project);
        assert!(project.find_effect_by_id(&id).unwrap().graph.is_some());

        let mut revert = RevertEffectGraphCommand::new(id.clone());
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

        let mut revert = RevertEffectGraphCommand::new(id.clone());
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
            id.clone(),
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
}
