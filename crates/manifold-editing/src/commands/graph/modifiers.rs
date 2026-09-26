//! Mesh-modifier stack commands (insert / remove / reorder) + their chain-walk
//! helpers. Split out of `graph.rs` in P2-G/S6 (pure move). Shared graph helpers
//! (target-graph access, descend_level, refresh_target_manifest, scene builders)
//! stay in `graph/mod.rs` and are reached via `super`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    PresetMetadata,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{SceneParamMetadata, stamp_scene_node_exposures_into};

use crate::command::Command;

use super::{
    descend_level, innermost_group_display_name, install_target_graph, refresh_target_manifest,
    resolve_target_instance, scene_build_node, scene_build_wire, with_existing_target_graph_mut,
    with_target_graph_mut, InstanceLayerSnapshot,
};

// ---------------------------------------------------------------------------
// Mesh-modifier stack (SCENE_SETUP_PANEL_DESIGN.md D6, P5): insert / remove /
// reorder a D6-curated single-mesh-in/mesh-out atom within an object's own
// group, splicing it into the `vertices` wire that feeds the group's
// `system.group_output` boundary. Shaped exactly like `AddSceneObjectCommand`
// / `AddSceneLightCommand`: one undoable composite that snapshots the WHOLE
// level (the object's group body) before mutating and restores it verbatim
// on undo, rather than reversing each wire edit by hand.
// ---------------------------------------------------------------------------

/// The curated D6 mesh-modifier vocabulary — the same 7 atoms
/// `scene_vm.rs`'s `MODIFIER_TYPE_IDS` curates for discovery, duplicated here
/// (this crate doesn't depend on `manifold-renderer`) — keep the two in sync
/// if either list changes.
pub(super) const MESH_MODIFIER_TYPE_IDS: &[&str] = &[
    "node.bend_mesh",
    "node.twist_mesh",
    "node.taper_mesh",
    "node.push_along_normals",
    "node.push_mesh",
    "node.morph_mesh",
    "node.rotate_3d",
];

fn resolve_level<'a>(
    def: &'a EffectGraphDef,
    scope: &[u32],
) -> Option<(&'a [EffectGraphNode], &'a [EffectGraphWire])> {
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    for id in scope {
        let group = nodes.iter().find(|node| node.id == *id)?.group.as_deref()?;
        nodes = &group.nodes;
        wires = &group.wires;
    }
    Some((nodes, wires))
}

/// Resolve the explicit owner supplied by the scene panel. A group owner edits
/// its body and group output; a bare scene object edits the current scope and
/// the object's `vertices` input directly.
pub(super) fn resolve_modifier_owner(
    def: &EffectGraphDef,
    scope_path: &[u32],
    owner_id: u32,
) -> Option<(Vec<u32>, u32, bool)> {
    let (nodes, _) = resolve_level(def, scope_path)?;
    let owner = nodes.iter().find(|node| node.id == owner_id)?;
    if owner.type_id == GROUP_TYPE_ID {
        let body = owner.group.as_deref()?;
        let output = body
            .nodes
            .iter()
            .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)?
            .id;
        let mut body_scope = scope_path.to_vec();
        body_scope.push(owner_id);
        return Some((body_scope, output, true));
    }
    (owner.type_id == "node.scene_object").then(|| (scope_path.to_vec(), owner_id, false))
}

fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| {
            node.id.max(
                node.group
                    .as_deref()
                    .map(|group| max_node_id_recursive(&group.nodes))
                    .unwrap_or(0),
            )
        })
        .max()
        .unwrap_or(0)
}

fn unique_wire_producer(
    wires: &[EffectGraphWire],
    to_node: u32,
    to_port: &str,
) -> Option<(u32, String)> {
    let mut matches = wires
        .iter()
        .filter(|wire| wire.to_node == to_node && wire.to_port == to_port);
    let wire = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some((wire.from_node, wire.from_port.clone()))
}

/// D12: find the `node.scene_object` bound at this level — the producer of
/// `group_out_id`'s `object` port. Mirrors `scene_vm.rs::find_scene_object_in_group`
/// (duplicated for the same cross-crate reason as `MESH_MODIFIER_TYPE_IDS`).
/// `None` when the level doesn't have this shape (unparseable/hand-edited
/// group) — callers must refuse the edit, never guess.
fn find_scene_object_at_group_output(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    group_out_id: u32,
) -> Option<u32> {
    let (producer_id, _) = unique_wire_producer(wires, group_out_id, "object")?;
    let node = nodes.iter().find(|n| n.id == producer_id)?;
    (node.type_id == "node.scene_object").then_some(producer_id)
}

/// `walk_mesh_modifier_chain`'s result: the modifier chain in wire order,
/// the mesh source's own `(node_id, port)`, and the scene_object id for the
/// import shape (`None` for the migrated/starter shape — see that
/// function's doc comment for the full duality).
pub(super) type ModifierChainWalk = (Vec<u32>, (u32, String), Option<u32>);
type RemoveModifierSnapshot = (
    Vec<EffectGraphNode>,
    Vec<EffectGraphWire>,
    Option<PresetMetadata>,
    Option<Option<EffectGraphDef>>,
    InstanceLayerSnapshot,
    Vec<String>,
);

/// Walk the D6 modifier chain feeding this group's mesh output, backward to
/// the mesh source — mirrors `scene_vm.rs::trace_scene_object`'s walk
/// (duplicated for the same cross-crate reason as `MESH_MODIFIER_TYPE_IDS`).
///
/// BUG-218/escape: two legitimate D12-era document shapes exist for the
/// group `full_modifier_scope` descends into, and both are committed forms
/// — NOT a fallback for malformed JSON (see `scene_vm.rs:617-618` and the
/// group-boundary-crossing walk around `scene_vm.rs:759`, which handle the
/// same duality):
///   1. **Import shape** (`AddSceneObjectCommand` / glTF importer): the
///      group's body contains its OWN `node.scene_object`, and the group
///      boundary re-exports only `object` — no `vertices` port at all. Walk
///      from the scene_object's own `vertices` INPUT port instead (resolved
///      via `find_scene_object_at_group_output`).
///   2. **Migrated/starter shape** (`migrate_scene_object_wires`, e.g. the
///      bundled `SceneStarter.json`): the minted `node.scene_object` stays a
///      ROOT-level SIBLING of this group rather than nested inside it — the
///      group's body is mesh+modifiers only and still re-exports `vertices`
///      directly via `system.group_output` (the pre-D12 shape, now feeding
///      a scene_object elsewhere instead of `render_scene` directly). Walk
///      from `group_out_id`'s own `vertices` OUTPUT port.
///
/// Which shape applies is resolved per-call: if `group_out_id`'s `object`
/// port has a `node.scene_object` producer (shape 1), use it; otherwise fall
/// through to shape 2's `vertices` port. Returns the chain in WIRE order
/// (source → … → output), the mesh source's own `(node_id, port)`, and
/// `Some(scene_object_id)` for shape 1 / `None` for shape 2 (splice's
/// terminal re-wire target — `None` means re-wire `group_out_id.vertices`
/// directly). `None` on anything unparseable in BOTH shapes (unwired
/// `vertices`, a dangling wire, a cycle) — every caller must refuse the edit
/// rather than guess a splice point, matching the Vm's own
/// `modifier_chain_parseable` posture.
pub(super) fn walk_mesh_modifier_chain_at(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    terminal_id: u32,
    group_output: bool,
) -> Option<ModifierChainWalk> {
    let scene_object_id = if group_output {
        find_scene_object_at_group_output(nodes, wires, terminal_id)
    } else {
        Some(terminal_id)
    };
    let mut chain_rev: Vec<u32> = Vec::new();
    let mut cursor = match scene_object_id {
        Some(id) => unique_wire_producer(wires, id, "vertices")?,
        None => unique_wire_producer(wires, terminal_id, "vertices")?,
    };
    loop {
        let (node_id, port) = cursor.clone();
        let node = nodes.iter().find(|n| n.id == node_id)?;
        if !MESH_MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
            chain_rev.reverse();
            return Some((chain_rev, (node_id, port), scene_object_id));
        }
        chain_rev.push(node_id);
        if chain_rev.len() > 64 {
            return None; // cycle guard.
        }
        if wires
            .iter()
            .filter(|wire| wire.from_node == node_id && wire.from_port == "out")
            .count()
            != 1
        {
            return None;
        }
        cursor = unique_wire_producer(wires, node_id, "in")?;
    }
}

/// Detach `node_id` (a modifier already present in `nodes`, currently wired
/// `in`/`out` inside the chain) from the chain: remove its two wires and
/// reconnect whoever fed it directly to whoever it fed — the node itself
/// stays in `nodes`, untouched. Shared by Remove (which then deletes the
/// node) and Move (which then re-splices the SAME node elsewhere). `None`
/// (refuse) if `node_id` isn't a modifier with exactly the expected in/out
/// wire shape.
fn detach_modifier(
    nodes: &[EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    node_id: u32,
) -> Option<()> {
    if !nodes
        .iter()
        .any(|n| n.id == node_id && MESH_MODIFIER_TYPE_IDS.contains(&n.type_id.as_str()))
    {
        return None;
    }
    if wires
        .iter()
        .filter(|wire| wire.to_node == node_id && wire.to_port == "in")
        .count()
        != 1
        || wires
            .iter()
            .filter(|wire| wire.from_node == node_id && wire.from_port == "out")
            .count()
            != 1
    {
        return None;
    }
    let pred_idx = wires
        .iter()
        .position(|wire| wire.to_node == node_id && wire.to_port == "in")?;
    let pred_wire = wires[pred_idx].clone();
    let succ_idx = wires
        .iter()
        .position(|w| w.from_node == node_id && w.from_port == "out")?;
    if pred_idx == succ_idx {
        return None;
    }
    let succ = wires.remove(succ_idx);
    wires.remove(if pred_idx < succ_idx { pred_idx } else { pred_idx - 1 });
    let reconnect_at = pred_idx.min(succ_idx);
    wires.insert(reconnect_at, scene_build_wire(
        pred_wire.from_node,
        &pred_wire.from_port,
        succ.to_node,
        &succ.to_port,
    ));
    Some(())
}

/// Splice `node_id` (already present in `nodes`, NOT currently wired into the
/// chain) into the chain feeding this group's mesh output at `position` (D6:
/// `0` = just after the mesh source; `None` = end of stack, just before the
/// terminal port — clamped to the chain's length). Shared by Insert (a
/// freshly created node) and Move (an existing node, freshly detached by
/// `detach_modifier`). BUG-218/escape: the terminal re-wire target follows
/// `walk_mesh_modifier_chain`'s resolved shape — the scene_object's own
/// `vertices` INPUT port for the import shape (`Some(id)`), or
/// `group_out_id`'s own `vertices` OUTPUT port for the migrated/starter
/// shape (`None`) — see that function's doc comment for the full duality.
pub(super) fn splice_modifier_into_chain_at(
    nodes: &[EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    terminal_id: u32,
    group_output: bool,
    node_id: u32,
    position: Option<usize>,
) -> Option<()> {
    let (chain, mesh_source, scene_object_id) =
        walk_mesh_modifier_chain_at(nodes, wires, terminal_id, group_output)?;
    let p = position.unwrap_or(chain.len()).min(chain.len());
    let (pred_node, pred_port) = if p == 0 {
        mesh_source
    } else {
        (chain[p - 1], "out".to_string())
    };
    let (succ_node, succ_port) = if p < chain.len() {
        (chain[p], "in".to_string())
    } else {
        match scene_object_id {
            Some(id) => (id, "vertices".to_string()),
            None => (terminal_id, "vertices".to_string()),
        }
    };
    let idx = wires.iter().position(|w| {
        w.from_node == pred_node
            && w.from_port == pred_port
            && w.to_node == succ_node
            && w.to_port == succ_port
    })?;
    wires.splice(
        idx..=idx,
        [
            scene_build_wire(pred_node, &pred_port, node_id, "in"),
            scene_build_wire(node_id, "out", succ_node, &succ_port),
        ],
    );
    Some(())
}

/// Insert a new D6 modifier node into an object's mesh chain (D6). One undo
/// unit: undo restores the object group's whole body (nodes + wires) exactly
/// as it stood before the insert.
#[derive(Debug)]
pub struct InsertMeshModifierCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    type_id: String,
    /// `None` = append at the end of the stack (D6 default); `Some(0)` =
    /// just after the mesh source.
    position: Option<usize>,
    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new modifier
    /// node's full param manifest, computed by the app-side caller via
    /// `manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(&type_id)`
    /// (this crate has no renderer dep).
    modifier_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The object group body's `(nodes, wires)` before this edit, plus the
    /// pre-edit whole-def `preset_metadata` (exposures land there, outside
    /// the scoped level). Set on execute.
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    prev_graph: Option<Option<EffectGraphDef>>,
    prev_instance: Option<super::InstanceLayerSnapshot>,
    created_node_id: Option<manifold_core::NodeId>,
    rejection: Option<&'static str>,
}

impl InsertMeshModifierCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        type_id: String,
        position: Option<usize>,
        modifier_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        debug_assert!(
            MESH_MODIFIER_TYPE_IDS.contains(&type_id.as_str()),
            "InsertMeshModifierCommand takes only the D6 curated vocabulary"
        );
        Self {
            target,
            scope_path,
            group_node_id,
            type_id,
            position,
            modifier_metadata,
            catalog_default,
            prev: None,
            prev_graph: None,
            prev_instance: None,
            created_node_id: None,
            rejection: None,
        }
    }
}

/// Human-readable label for a mesh-modifier atom's card section — mirrors
/// `manifold_renderer::node_graph::scene_exposure::section_name_for_node`'s
/// modifier fallback convention (duplicated: this crate has no renderer dep,
/// same reason `MESH_MODIFIER_TYPE_IDS` above is duplicated).
fn modifier_section_label(type_id: &str) -> String {
    type_id
        .strip_prefix("node.")
        .map(|s| {
            let mut s = s.to_string();
            s.replace_range(0..1, &s[0..1].to_uppercase());
            s
        })
        .unwrap_or_else(|| "Modifier".to_string())
}

impl Command for InsertMeshModifierCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.prev_graph = None;
        self.prev_instance = None;
        self.rejection = None;
        let previous_instance = project
            .preset_instance(&self.target)
            .map(InstanceLayerSnapshot::capture);
        let previous_graph = project
            .graph_target_owner(&self.target)
            .map(|owner| owner.graph.clone());
        let base = previous_graph.clone().flatten().unwrap_or_else(|| self.catalog_default.clone());
        let Some((scope, terminal, group_output)) =
            resolve_modifier_owner(&base, &self.scope_path, self.group_node_id)
        else {
            self.rejection = Some("Modifier owner is unavailable");
            return;
        };
        let Some((nodes, wires)) = resolve_level(&base, &scope) else {
            self.rejection = Some("Modifier scope is unavailable");
            return;
        };
        if walk_mesh_modifier_chain_at(nodes, wires, terminal, group_output).is_none() {
            self.rejection = Some("Modifier chain is malformed");
            return;
        }
        let Some(new_id) = max_node_id_recursive(&base.nodes).checked_add(1) else {
            self.rejection = Some("Modifier node id space is exhausted");
            return;
        };
        let mut new_node = scene_build_node(new_id, &self.type_id, None, BTreeMap::new());
        let new_node_id = self
            .created_node_id
            .clone()
            .unwrap_or_else(|| new_node.node_id.clone());
        new_node.node_id = new_node_id.clone();
        let prev_metadata = base.preset_metadata.clone();
        let mut candidate = base.clone();
        let Some((candidate_nodes, candidate_wires)) =
            descend_level(&mut candidate.nodes, &mut candidate.wires, &scope)
        else {
            self.rejection = Some("Modifier scope is unavailable");
            return;
        };
        let prev = (candidate_nodes.clone(), candidate_wires.clone());
        candidate_nodes.push(new_node);
        if splice_modifier_into_chain_at(
            candidate_nodes,
            candidate_wires,
            terminal,
            group_output,
            new_id,
            self.position,
        )
        .is_none()
        {
            self.rejection = Some("Modifier chain is malformed");
            return;
        }
        let section = if group_output {
            innermost_group_display_name(&candidate.nodes, &scope)
        } else {
            resolve_level(&candidate, &scope)
                .and_then(|(level, _)| level.iter().find(|node| node.id == terminal))
                .and_then(|node| node.handle.clone())
        }
        .map(|name| format!("{name} — {}", modifier_section_label(&self.type_id)))
        .unwrap_or_else(|| modifier_section_label(&self.type_id));
        let meta = candidate.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
            display_name: "Scene".to_string(), category: "Geometry".to_string(),
            osc_prefix: "scene".to_string(), legacy_discriminant: None, available: true,
            is_line_based: false, layer_types: None, params: Vec::new(), bindings: Vec::new(),
            param_aliases: Vec::new(), value_aliases: Vec::new(), string_params: Vec::new(),
            string_bindings: Vec::new(), scene_modifier: None, scene_bounds: None,
        });
        stamp_scene_node_exposures_into(
            &mut meta.params, &mut meta.bindings, new_id, &new_node_id, &self.type_id,
            &section, &self.modifier_metadata, &BTreeMap::new(),
        );
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            *def = candidate;
        }).is_none() {
            self.rejection = Some("Modifier target is unavailable");
            return;
        }
        self.prev = Some((prev.0, prev.1, prev_metadata));
        self.prev_graph = previous_graph;
        self.prev_instance = previous_instance;
        self.created_node_id = Some(new_node_id);
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        if matches!(self.prev_graph, Some(None)) {
            install_target_graph(project, &self.target, None);
        } else {
            let Some((scope, _, _)) = project
                .graph_for_target(&self.target, None)
                .and_then(|def| resolve_modifier_owner(def, &self.scope_path, self.group_node_id))
            else { return; };
            let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
                def.preset_metadata = pmeta;
                if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                    *nodes = pn;
                    *wires = pw;
                }
            });
        }
        if let (Some(snapshot), Some(instance)) = (self.prev_instance.take(), resolve_target_instance(&self.target, project)) {
            snapshot.restore(instance);
        }
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Insert Modifier"
    }

    fn was_applied(&self) -> bool { self.prev.is_some() }

    fn rejection_reason(&self) -> Option<&str> { self.rejection }
}

/// Remove one D6 modifier node from an object's mesh chain, rejoining the
/// wire around it (D6: "unsplice + delete"). One undo unit.
#[derive(Debug)]
pub struct RemoveMeshModifierCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    modifier_node_id: u32,
    catalog_default: EffectGraphDef,
    prev: Option<RemoveModifierSnapshot>,
    rejection: Option<&'static str>,
}

impl RemoveMeshModifierCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        modifier_node_id: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            modifier_node_id,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }
}

impl Command for RemoveMeshModifierCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let previous_instance = project
            .preset_instance(&self.target)
            .map(InstanceLayerSnapshot::capture);
        let Some(previous_instance) = previous_instance else {
            self.rejection = Some("Modifier target is unavailable");
            return;
        };
        let previous_graph = project.graph_target_owner(&self.target).map(|owner| owner.graph.clone());
        let Some(base) = previous_graph.clone().flatten() else {
            self.rejection = Some("Modifier graph is unavailable"); return;
        };
        let Some((scope, terminal, group_output)) = resolve_modifier_owner(&base, &self.scope_path, self.group_node_id) else {
            self.rejection = Some("Modifier owner is unavailable"); return;
        };
        let Some((nodes, wires)) = resolve_level(&base, &scope) else {
            self.rejection = Some("Modifier scope is unavailable"); return;
        };
        let Some((chain, _, _)) = walk_mesh_modifier_chain_at(nodes, wires, terminal, group_output) else {
            self.rejection = Some("Modifier chain is malformed"); return;
        };
        if !chain.contains(&self.modifier_node_id) {
            self.rejection = Some("Modifier is not in the selected object's chain"); return;
        }
        if wires.iter().filter(|wire| wire.to_node == self.modifier_node_id && wire.to_port == "in").count() != 1
            || wires.iter().filter(|wire| wire.from_node == self.modifier_node_id && wire.from_port == "out").count() != 1
        {
            self.rejection = Some("Modifier chain has a fanout or malformed link"); return;
        }
        let mut candidate = base.clone();
        let Some((candidate_nodes, candidate_wires)) = descend_level(&mut candidate.nodes, &mut candidate.wires, &scope) else {
            self.rejection = Some("Modifier scope is unavailable"); return;
        };
        let previous_nodes = candidate_nodes.clone();
        let previous_wires = candidate_wires.clone();
        if detach_modifier(candidate_nodes, candidate_wires, self.modifier_node_id).is_none() {
            self.rejection = Some("Modifier chain is malformed"); return;
        }
        candidate_nodes.retain(|node| node.id != self.modifier_node_id);
        candidate_wires.retain(|wire| wire.from_node != self.modifier_node_id && wire.to_node != self.modifier_node_id);
        let removed_node_ids = previous_nodes.iter()
            .find(|node| node.id == self.modifier_node_id)
            .map(|node| vec![node.node_id.clone()]).unwrap_or_default();
        let removed_params = super::scene::prune_scene_object_metadata(&mut candidate, &removed_node_ids);
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| *def = candidate).is_none() {
            self.rejection = Some("Modifier target is unavailable"); return;
        }
        refresh_target_manifest(project, &self.target);
        if let Some(instance) = resolve_target_instance(&self.target, project) {
            super::prune_instance_params(instance, &removed_params);
        }
        self.prev = Some((previous_nodes, previous_wires, base.preset_metadata.clone(), previous_graph, previous_instance, removed_params));
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta, pgraph, snapshot, _removed)) = self.prev.take() else {
            return;
        };
        if matches!(pgraph, Some(Some(_))) {
            let Some((scope, _, _)) = project.graph_for_target(&self.target, None)
                .and_then(|def| resolve_modifier_owner(def, &self.scope_path, self.group_node_id)) else { return; };
            let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
                def.preset_metadata = pmeta;
                if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                    *nodes = pn; *wires = pw;
                }
            });
        } else {
            install_target_graph(project, &self.target, pgraph.flatten());
        }
        if let Some(instance) = resolve_target_instance(&self.target, project) { snapshot.restore(instance); }
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Remove Modifier"
    }

    fn was_applied(&self) -> bool { self.prev.is_some() }

    fn rejection_reason(&self) -> Option<&str> { self.rejection }
}

/// Reorder one D6 modifier node within an object's mesh chain (D6: "unsplice
/// and resplice"). `new_position` uses the same 0-based convention as
/// `InsertMeshModifierCommand::position` — position zero means just after
/// the mesh source — measured against the stack WITHOUT the moved node;
/// moving the last modifier "down" or the first "up" is a harmless no-op
/// (clamped by `splice_modifier_into_chain`). One undo unit.
#[derive(Debug)]
pub struct MoveMeshModifierCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    modifier_node_id: u32,
    new_position: usize,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
    rejection: Option<&'static str>,
}

impl MoveMeshModifierCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        modifier_node_id: u32,
        new_position: usize,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            modifier_node_id,
            new_position,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }
}

impl Command for MoveMeshModifierCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(base) = project.graph_for_target(&self.target, None).cloned() else {
            self.rejection = Some("Modifier graph is unavailable"); return;
        };
        let Some((scope, terminal, group_output)) = resolve_modifier_owner(&base, &self.scope_path, self.group_node_id) else {
            self.rejection = Some("Modifier owner is unavailable"); return;
        };
        let Some((nodes, wires)) = resolve_level(&base, &scope) else {
            self.rejection = Some("Modifier scope is unavailable"); return;
        };
        let Some((chain, _, _)) = walk_mesh_modifier_chain_at(nodes, wires, terminal, group_output) else {
            self.rejection = Some("Modifier chain is malformed"); return;
        };
        let Some(old_position) = chain.iter().position(|id| *id == self.modifier_node_id) else {
            self.rejection = Some("Modifier is not in the selected object's chain"); return;
        };
        let target_position = self.new_position.min(chain.len().saturating_sub(1));
        if target_position == old_position { return; }
        let mut candidate = base.clone();
        let Some((candidate_nodes, candidate_wires)) = descend_level(&mut candidate.nodes, &mut candidate.wires, &scope) else {
            self.rejection = Some("Modifier scope is unavailable"); return;
        };
        let previous = (candidate_nodes.clone(), candidate_wires.clone());
        if detach_modifier(candidate_nodes, candidate_wires, self.modifier_node_id).is_none()
            || splice_modifier_into_chain_at(candidate_nodes, candidate_wires, terminal, group_output, self.modifier_node_id, Some(self.new_position)).is_none()
        {
            self.rejection = Some("Modifier chain is malformed"); return;
        }
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| *def = candidate).is_none() {
            self.rejection = Some("Modifier target is unavailable"); return;
        }
        self.prev = Some(previous);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let Some(def) = project.graph_for_target(&self.target, None) else { return; };
        let Some((scope, _, _)) = resolve_modifier_owner(def, &self.scope_path, self.group_node_id) else { return; };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Reorder Modifier"
    }

    fn was_applied(&self) -> bool { self.prev.is_some() }

    fn rejection_reason(&self) -> Option<&str> { self.rejection }
}

#[cfg(test)]
mod tests;
