//! Mesh-modifier stack commands (insert / remove / reorder) + their chain-walk
//! helpers. Split out of `graph.rs` in P2-G/S6 (pure move). Shared graph helpers
//! (target-graph access, descend_level, refresh_target_manifest, scene builders)
//! stay in `graph/mod.rs` and are reached via `super`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, PresetMetadata,
    SkipModeDef,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{stamp_scene_node_exposures_into, SceneParamMetadata};

use crate::command::Command;

use super::{
    descend_level, innermost_group_display_name, refresh_target_manifest, scene_build_node,
    scene_build_wire, with_existing_target_graph_mut, with_target_graph_mut,
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

/// `scope_path` (the level holding the object's group) + `group_node_id` (the
/// object's own group at that level) → the full descend path to the group's
/// BODY — one level deeper, where the mesh chain and its wires actually live.
fn full_modifier_scope(scope_path: &[u32], group_node_id: u32) -> Vec<u32> {
    let mut s = scope_path.to_vec();
    s.push(group_node_id);
    s
}

/// The (node_id, port) wired INTO `(to_node, to_port)`, if any.
fn wire_producer(wires: &[EffectGraphWire], to_node: u32, to_port: &str) -> Option<(u32, String)> {
    wires
        .iter()
        .find(|w| w.to_node == to_node && w.to_port == to_port)
        .map(|w| (w.from_node, w.from_port.clone()))
}

/// Remove and return the wire feeding `(to_node, to_port)`, if any.
fn remove_wire_into(wires: &mut Vec<EffectGraphWire>, to_node: u32, to_port: &str) -> Option<(u32, String)> {
    let idx = wires.iter().position(|w| w.to_node == to_node && w.to_port == to_port)?;
    let w = wires.remove(idx);
    Some((w.from_node, w.from_port))
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
    let (producer_id, _) = wire_producer(wires, group_out_id, "object")?;
    let node = nodes.iter().find(|n| n.id == producer_id)?;
    (node.type_id == "node.scene_object").then_some(producer_id)
}

/// `walk_mesh_modifier_chain`'s result: the modifier chain in wire order,
/// the mesh source's own `(node_id, port)`, and the scene_object id for the
/// import shape (`None` for the migrated/starter shape — see that
/// function's doc comment for the full duality).
type ModifierChainWalk = (Vec<u32>, (u32, String), Option<u32>);

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
fn walk_mesh_modifier_chain(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    group_out_id: u32,
) -> Option<ModifierChainWalk> {
    let scene_object_id = find_scene_object_at_group_output(nodes, wires, group_out_id);
    let mut chain_rev: Vec<u32> = Vec::new();
    let mut cursor = match scene_object_id {
        Some(id) => wire_producer(wires, id, "vertices")?,
        None => wire_producer(wires, group_out_id, "vertices")?,
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
        cursor = wire_producer(wires, node_id, "in")?;
    }
}

/// Detach `node_id` (a modifier already present in `nodes`, currently wired
/// `in`/`out` inside the chain) from the chain: remove its two wires and
/// reconnect whoever fed it directly to whoever it fed — the node itself
/// stays in `nodes`, untouched. Shared by Remove (which then deletes the
/// node) and Move (which then re-splices the SAME node elsewhere). `None`
/// (refuse) if `node_id` isn't a modifier with exactly the expected in/out
/// wire shape.
fn detach_modifier(nodes: &[EffectGraphNode], wires: &mut Vec<EffectGraphWire>, node_id: u32) -> Option<()> {
    if !nodes
        .iter()
        .any(|n| n.id == node_id && MESH_MODIFIER_TYPE_IDS.contains(&n.type_id.as_str()))
    {
        return None;
    }
    let (pred_node, pred_port) = remove_wire_into(wires, node_id, "in")?;
    let succ_idx = wires.iter().position(|w| w.from_node == node_id && w.from_port == "out")?;
    let succ = wires.remove(succ_idx);
    wires.push(scene_build_wire(pred_node, &pred_port, succ.to_node, &succ.to_port));
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
fn splice_modifier_into_chain(
    nodes: &[EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    group_out_id: u32,
    node_id: u32,
    position: Option<usize>,
) -> Option<()> {
    let (chain, mesh_source, scene_object_id) = walk_mesh_modifier_chain(nodes, wires, group_out_id)?;
    let p = position.unwrap_or(chain.len()).min(chain.len());
    let (pred_node, pred_port) = if p == 0 { mesh_source } else { (chain[p - 1], "out".to_string()) };
    let (succ_node, succ_port) = if p < chain.len() {
        (chain[p], "in".to_string())
    } else {
        match scene_object_id {
            Some(id) => (id, "vertices".to_string()),
            None => (group_out_id, "vertices".to_string()),
        }
    };
    let idx = wires.iter().position(|w| {
        w.from_node == pred_node && w.from_port == pred_port && w.to_node == succ_node && w.to_port == succ_port
    })?;
    wires.remove(idx);
    wires.push(scene_build_wire(pred_node, &pred_port, node_id, "in"));
    wires.push(scene_build_wire(node_id, "out", succ_node, &succ_port));
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
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
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
    fn execute(&mut self, project: &mut Project) {
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
        let type_id = self.type_id.clone();
        let position = self.position;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let prev_metadata = def.preset_metadata.clone();
            // The object group's own display name prefixes the section
            // (e.g. "Object 1 — Bend"), mirroring the importer's modifier
            // section convention — computed BEFORE the nested block below so
            // this read of `def.nodes` doesn't overlap the block's `&mut`.
            let section = match innermost_group_display_name(&def.nodes, &scope) {
                Some(group_name) => format!("{group_name} — {}", modifier_section_label(&type_id)),
                None => modifier_section_label(&type_id),
            };

            let (new_id, new_node_id, prev) = {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let out_id = nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?.id;
                // Validate the chain is parseable BEFORE mutating anything — a
                // custom/unparseable chain refuses the insert (D6), never a
                // blind splice.
                walk_mesh_modifier_chain(nodes, wires, out_id)?;
                let prev = (nodes.clone(), wires.clone());
                let new_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                let new_node = scene_build_node(new_id, &type_id, None, BTreeMap::new());
                let new_node_id = new_node.node_id.clone();
                nodes.push(new_node);
                splice_modifier_into_chain(nodes, wires, out_id, new_id, position)
                    .expect("chain re-validated above via walk_mesh_modifier_chain; splice cannot fail here");
                (new_id, new_node_id, prev)
            };

            // P1: expose every param of the freshly minted modifier node,
            // into the def's TOP-LEVEL preset_metadata, targeting its bare
            // NodeId — same convention the glTF importer uses.
            let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
                id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
                display_name: "Scene".to_string(),
                category: "Geometry".to_string(),
                osc_prefix: "scene".to_string(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: Vec::new(),
                skip_mode: SkipModeDef::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            });
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                new_id,
                &new_node_id,
                &section,
                &self.modifier_metadata,
                &BTreeMap::new(),
            );

            Some((prev, prev_metadata))
        });
        if let Some((pnw, pmeta)) = result.flatten() {
            self.prev = Some((pnw.0, pnw.1, pmeta));
        }
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = pmeta;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Insert Modifier"
    }
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
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
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
        }
    }
}

impl Command for RemoveMeshModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
        let modifier_id = self.modifier_node_id;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());
            detach_modifier(nodes, wires, modifier_id)?;
            nodes.retain(|n| n.id != modifier_id);
            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Remove Modifier"
    }
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
        }
    }
}

impl Command for MoveMeshModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
        let modifier_id = self.modifier_node_id;
        let new_position = self.new_position;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let out_id = nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?.id;
            let (chain, _, _) = walk_mesh_modifier_chain(nodes, wires, out_id)?;
            if !chain.contains(&modifier_id) {
                return None; // not a member of THIS object's chain — refuse.
            }
            let prev = (nodes.clone(), wires.clone());
            detach_modifier(nodes, wires, modifier_id)?;
            splice_modifier_into_chain(nodes, wires, out_id, modifier_id, Some(new_position))?;
            Some(prev)
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = full_modifier_scope(&self.scope_path, self.group_node_id);
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
}


#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::test_support::*;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
    use manifold_core::effect_graph_def::{GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef};
    use crate::command::Command;
    use super::super::modifiers::MESH_MODIFIER_TYPE_IDS;

    fn plain_node(id: u32, handle: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(handle),
            type_id: type_id.to_string(),
            handle: Some(handle.to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    /// A one-object scene: `render_scene` (id 0, `objects=1`) wired to a
    /// named group (id 1) containing a mesh source (id 10) → the given
    /// modifier chain (ids 11, 12, … in wire order) → `node.scene_object`
    /// (id 90) → `system.group_output` (id 99, re-exporting `object` only)
    /// — the real D12 `AddSceneObjectCommand`/importer shape (see
    /// `AddSceneObjectCommand::execute`), close enough to exercise the
    /// splice commands against a realistic nested-group body. BUG-218: this
    /// fixture used to construct the pre-D12 shape (group_output's own
    /// `vertices` port re-exported directly, no scene_object at all) — that
    /// shape never reproduced the bug the commands actually hit against
    /// real objects, so it's replaced wholesale rather than kept alongside.
    fn object_group_scene(modifier_type_ids: &[&str]) -> EffectGraphDef {
        let mesh = plain_node(10, "mesh", "node.cube_mesh");
        let mut body_nodes = vec![mesh];
        let mut body_wires = Vec::new();
        let mut prev = (10u32, "vertices".to_string());
        for (i, type_id) in modifier_type_ids.iter().enumerate() {
            let id = 11 + i as u32;
            body_nodes.push(plain_node(id, &format!("mod{i}"), type_id));
            body_wires.push(scene_build_wire(prev.0, &prev.1, id, "in"));
            prev = (id, "out".to_string());
        }
        let scene_object_id = 90;
        let scene_object = plain_node(scene_object_id, "Hero", "node.scene_object");
        body_wires.push(scene_build_wire(prev.0, &prev.1, scene_object_id, "vertices"));
        body_nodes.push(scene_object);

        let mut out_node = plain_node(99, "out", GROUP_OUTPUT_TYPE_ID);
        out_node.handle = None;
        body_wires.push(scene_build_wire(scene_object_id, "object", 99, "object"));
        body_nodes.push(out_node);

        let mut group_node = plain_node(1, "Hero", GROUP_TYPE_ID);
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: vec![InterfacePortDef {
                    name: "object".to_string(),
                    port_type: "Object".to_string(),
                }],
                params: Vec::new(),
            },
            nodes: body_nodes,
            wires: body_wires,
            tint: None,
        }));

        let mut render = plain_node(0, "render", "node.render_scene");
        render.params.insert("objects".to_string(), SerializedParamValue::Float { value: 1.0 });

        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![group_node, render],
            wires: vec![scene_build_wire(1, "object", 0, "object_0")],
        }
    }

    /// A one-object scene in the OTHER legitimate D12-era shape —
    /// `migrate_scene_object_wires`'s output (e.g. the bundled
    /// `SceneStarter.json`): the mesh-producer group (id 1) contains ONLY
    /// mesh (id 10) → modifiers (ids 11, 12, …) → `system.group_output` (id
    /// 99, re-exporting `vertices` DIRECTLY — the pre-D12 shape). The minted
    /// `node.scene_object` (id 90) stays a ROOT-LEVEL SIBLING of the group,
    /// wired `group.vertices -> scene_object.vertices` (the migration's
    /// "same-scope re-point" — see `scene_object_migration.rs`'s
    /// `migrate_scope`, which only ever retargets the wire's `to_node`/
    /// `to_port`, never touches the group's own body). BUG-218/escape: the
    /// group being edited here has NO scene_object inside it and NO
    /// `object` port at all — `walk_mesh_modifier_chain` must fall through
    /// to walking the group output's own `vertices` port, matching the
    /// pre-D12 behavior this shape still relies on.
    fn migrated_object_group_scene(modifier_type_ids: &[&str]) -> EffectGraphDef {
        let mesh = plain_node(10, "mesh", "node.cube_mesh");
        let mut body_nodes = vec![mesh];
        let mut body_wires = Vec::new();
        let mut prev = (10u32, "vertices".to_string());
        for (i, type_id) in modifier_type_ids.iter().enumerate() {
            let id = 11 + i as u32;
            body_nodes.push(plain_node(id, &format!("mod{i}"), type_id));
            body_wires.push(scene_build_wire(prev.0, &prev.1, id, "in"));
            prev = (id, "out".to_string());
        }
        let mut out_node = plain_node(99, "out", GROUP_OUTPUT_TYPE_ID);
        out_node.handle = None;
        body_wires.push(scene_build_wire(prev.0, &prev.1, 99, "vertices"));
        body_nodes.push(out_node);

        let mut group_node = plain_node(1, "Hero", GROUP_TYPE_ID);
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: vec![InterfacePortDef {
                    name: "vertices".to_string(),
                    port_type: "Array(Vertex)".to_string(),
                }],
                params: Vec::new(),
            },
            nodes: body_nodes,
            wires: body_wires,
            tint: None,
        }));

        let scene_object = plain_node(90, "Hero", "node.scene_object");
        let mut render = plain_node(0, "render", "node.render_scene");
        render.params.insert("objects".to_string(), SerializedParamValue::Float { value: 1.0 });

        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![group_node, scene_object, render],
            wires: vec![
                scene_build_wire(1, "vertices", 90, "vertices"),
                scene_build_wire(90, "object", 0, "object_0"),
            ],
        }
    }

    /// Wrap `def`'s whole top level inside a fresh outer group at `outer_id`
    /// — the "nested-group placement" gate: the object's group (id 1) now
    /// lives at `scope_path = [outer_id]` instead of root, so
    /// `full_modifier_scope` must compose two hops, not one.
    fn wrap_in_outer_group(def: EffectGraphDef, outer_id: u32) -> EffectGraphDef {
        let mut outer = plain_node(outer_id, "Outer", GROUP_TYPE_ID);
        outer.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: Vec::new(), outputs: Vec::new(), params: Vec::new() },
            nodes: def.nodes,
            wires: def.wires,
            tint: None,
        }));
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![outer],
            wires: Vec::new(),
        }
    }

    /// Read the modifier stack's node ids back off `def`, in wire order —
    /// the "Vm chain-trace tests: stack order matches wire order" gate,
    /// re-derived independently of `scene_vm.rs` (this crate can't depend on
    /// it) by walking the SAME chain shape production code walks. BUG-218/
    /// escape: mirrors `walk_mesh_modifier_chain`'s dual-shape resolution —
    /// if the group output's `object` port resolves to a scene_object,
    /// anchor on ITS `vertices` input (import shape); otherwise anchor on
    /// the group output's own `vertices` port directly (migrated/starter
    /// shape, `scene_vm.rs:617-618`).
    fn modifier_ids_in_wire_order(def: &EffectGraphDef, scope: &[u32]) -> Vec<u32> {
        let mut nodes: &[EffectGraphNode] = &def.nodes;
        let mut wires: &[EffectGraphWire] = &def.wires;
        for gid in scope {
            let group = nodes.iter().find(|n| n.id == *gid).unwrap();
            let body = group.group.as_deref().unwrap();
            nodes = &body.nodes;
            wires = &body.wires;
        }
        let out_id = nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID).unwrap().id;
        let scene_object_id = wires.iter().find(|w| w.to_node == out_id && w.to_port == "object").map(|w| w.from_node);
        let anchor = scene_object_id.unwrap_or(out_id);
        let mut chain = Vec::new();
        let mut cursor = wires
            .iter()
            .find(|w| w.to_node == anchor && w.to_port == "vertices")
            .map(|w| (w.from_node, w.from_port.clone()));
        while let Some((node_id, _)) = cursor {
            let node = nodes.iter().find(|n| n.id == node_id).unwrap();
            if !MESH_MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
                break;
            }
            chain.push(node_id);
            cursor = wires
                .iter()
                .find(|w| w.to_node == node_id && w.to_port == "in")
                .map(|w| (w.from_node, w.from_port.clone()));
        }
        chain.reverse();
        chain
    }

    #[test]
    fn insert_modifier_appends_to_empty_stack_and_undo_restores() {
        let (mut project, fx) = project_with_graph(object_group_scene(&[]));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            None,
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 1);
        let inserted = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap().nodes.iter().find(|n| n.id == ids[0]).unwrap();
        assert_eq!(inserted.type_id, "node.bend_mesh");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-insert graph exactly (inverse-pair)");
    }

    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `InsertMeshModifierCommand`
    /// stamps the caller-supplied modifier metadata into the def's TOP-LEVEL
    /// `preset_metadata`, targeting the inserted node's bare `NodeId`,
    /// section named `"{object group name} — {modifier label}"` (mirrors the
    /// glTF importer's modifier section convention, duplicated in
    /// `modifier_section_label` since this crate has no renderer dep). Undo
    /// restores `preset_metadata` verbatim; execute→undo→redo is
    /// structurally stable (see the AddSceneObjectCommand sibling test for
    /// why redo isn't byte-identical: `execute` mints a fresh random NodeId
    /// every call).
    #[test]
    fn insert_modifier_stamps_exposures_and_undo_redo_are_stable() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(object_group_scene(&[]));

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            None,
            vec![scene_param_meta("amount", "Amount")],
            mirror_catalog_default(),
        );

        let assert_stamped = |project: &Project| {
            let def = graph_of(project, &fx);
            let ids = modifier_ids_in_wire_order(def, &[1]);
            assert_eq!(ids.len(), 1);
            let inserted = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap().nodes.iter().find(|n| n.id == ids[0]).unwrap();
            assert_eq!(inserted.type_id, "node.bend_mesh");

            let meta = def.preset_metadata.as_ref().expect("P1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 1);
            assert_eq!(
                meta.params[0].section.as_deref(),
                Some("Hero — Bend_mesh"),
                "section = '{{object group name}} — {{modifier label}}' (the fixture's group is named 'Hero')"
            );
            assert!(
                meta.bindings.iter().any(|b| matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == inserted.node_id && param == "amount"
                )),
                "modifier exposure targets the inserted node's bare NodeId"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(def.preset_metadata.is_none(), "undo restores the pre-insert (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
    }

    /// BUG-218 escape: the migrated/starter shape (`migrate_scene_object_wires`
    /// — the scene_object lives OUTSIDE this group, the group only exports
    /// `vertices`) must still splice via the group output's `vertices` port,
    /// not the import shape's scene_object-input anchor. Regression gate for
    /// the escape found landing this fix: the earlier version of this fix
    /// only handled the import shape and silently broke this one.
    #[test]
    fn insert_modifier_on_migrated_shape_splices_at_group_output_and_undo_restores() {
        let (mut project, fx) = project_with_graph(migrated_object_group_scene(&[]));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            None,
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 1, "the migrated shape's group output still gains the modifier");
        let inserted = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap().nodes.iter().find(|n| n.id == ids[0]).unwrap();
        assert_eq!(inserted.type_id, "node.bend_mesh");
        // The root-level scene_object (id 90) is untouched — its own
        // `vertices` wire still comes straight from the group's boundary.
        assert!(
            def.wires.iter().any(|w| w.from_node == 1 && w.from_port == "vertices" && w.to_node == 90 && w.to_port == "vertices"),
            "scene_object still wired directly from the group's vertices boundary"
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-insert graph exactly (inverse-pair), migrated shape");
    }

    /// Companion to the insert gate above: Remove/Move on the migrated shape
    /// also splice at the group output, not a (nonexistent, in this shape)
    /// scene_object input.
    #[test]
    fn remove_and_move_modifier_on_migrated_shape_splice_at_group_output_and_undo_restores() {
        let (mut project, fx) =
            project_with_graph(migrated_object_group_scene(&["node.bend_mesh", "node.twist_mesh", "node.taper_mesh"]));
        let before = graph_of(&project, &fx).clone();
        let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist, taper]

        let mut remove_cmd = RemoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[1], // remove the middle (twist)
            mirror_catalog_default(),
        );
        remove_cmd.execute(&mut project);
        let after_remove = graph_of(&project, &fx);
        assert_eq!(modifier_ids_in_wire_order(after_remove, &[1]), vec![ids0[0], ids0[2]]);
        remove_cmd.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &before, "undo restores after removing the middle, migrated shape");

        let mut move_cmd = MoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[2], // move taper (last) to the front
            0,
            mirror_catalog_default(),
        );
        move_cmd.execute(&mut project);
        let after_move = graph_of(&project, &fx);
        assert_eq!(
            modifier_ids_in_wire_order(after_move, &[1]),
            vec![ids0[2], ids0[0], ids0[1]],
            "taper moved to the front, migrated shape"
        );
        move_cmd.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &before, "undo restores after the move, migrated shape");
    }

    #[test]
    fn insert_modifier_at_position_zero_lands_just_after_mesh_source() {
        let (mut project, fx) = project_with_graph(object_group_scene(&["node.twist_mesh"]));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            Some(0),
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 2, "one existing + one inserted");
        let group = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap();
        let first = group.nodes.iter().find(|n| n.id == ids[0]).unwrap();
        let second = group.nodes.iter().find(|n| n.id == ids[1]).unwrap();
        assert_eq!(first.type_id, "node.bend_mesh", "position 0 = just after the mesh source");
        assert_eq!(second.type_id, "node.twist_mesh", "the pre-existing modifier now sits second");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-insert graph exactly (inverse-pair)");
    }

    #[test]
    fn insert_modifier_default_position_appends_at_the_end() {
        let (mut project, fx) = project_with_graph(object_group_scene(&["node.twist_mesh", "node.taper_mesh"]));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            None,
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 3);
        let group = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap();
        let last = group.nodes.iter().find(|n| n.id == ids[2]).unwrap();
        assert_eq!(last.type_id, "node.bend_mesh", "no position = end of stack, just before the group output");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-insert graph exactly (inverse-pair)");
    }

    #[test]
    fn insert_modifier_in_nested_group_composes_scope_path() {
        // The object's own group (id 1) now lives at scope_path [50] instead
        // of root — proves `full_modifier_scope` composes two hops.
        let (mut project, fx) = project_with_graph(wrap_in_outer_group(object_group_scene(&[]), 50));
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![50],
            1,
            "node.rotate_3d".to_string(),
            None,
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[50, 1]);
        assert_eq!(ids.len(), 1);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-insert graph exactly (inverse-pair), nested case");
    }

    #[test]
    fn insert_modifier_refuses_on_unparseable_chain() {
        // A group whose `vertices` boundary is unwired entirely — the
        // command must refuse (no partial/corrupt mutation), matching the
        // Vm's own `modifier_chain_parseable = false` case.
        let mut def = object_group_scene(&[]);
        {
            let group = def.nodes.iter_mut().find(|n| n.id == 1).unwrap();
            group.group.as_mut().unwrap().wires.clear(); // vertices now unwired
        }
        let (mut project, fx) = project_with_graph(def);
        let before = graph_of(&project, &fx).clone();

        let mut cmd = InsertMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            "node.bend_mesh".to_string(),
            None,
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "an unparseable chain is refused — no node pushed, no wires touched");
    }

    #[test]
    fn remove_modifier_middle_of_stack_rejoins_wire_and_undo_restores() {
        let (mut project, fx) =
            project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh", "node.taper_mesh"]));
        let before = graph_of(&project, &fx).clone();
        let middle_id = modifier_ids_in_wire_order(&before, &[1])[1];

        let mut cmd = RemoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            middle_id,
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 2, "the middle modifier is gone");
        assert!(!ids.contains(&middle_id));
        let group = def.nodes.iter().find(|n| n.id == 1).unwrap().group.as_deref().unwrap();
        assert!(!group.nodes.iter().any(|n| n.id == middle_id), "the node itself is deleted");
        assert_eq!(
            group.nodes.iter().find(|n| n.id == ids[0]).unwrap().type_id,
            "node.bend_mesh"
        );
        assert_eq!(
            group.nodes.iter().find(|n| n.id == ids[1]).unwrap().type_id,
            "node.taper_mesh",
            "bend now feeds taper directly — the wire rejoined around the removed node"
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-remove graph exactly (inverse-pair)");
    }

    #[test]
    fn remove_modifier_at_first_and_last_positions_and_undo_restores() {
        let (mut project, fx) = project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh"]));
        let before = graph_of(&project, &fx).clone();
        let ids0 = modifier_ids_in_wire_order(&before, &[1]);

        // Remove the FIRST modifier — mesh source must now feed the second directly.
        let mut cmd_first = RemoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[0],
            mirror_catalog_default(),
        );
        cmd_first.execute(&mut project);
        let after_first = graph_of(&project, &fx);
        assert_eq!(modifier_ids_in_wire_order(after_first, &[1]), vec![ids0[1]]);
        cmd_first.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &before, "undo restores after removing the first");

        // Remove the LAST modifier — its predecessor must now feed group_output directly.
        let mut cmd_last = RemoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[1],
            mirror_catalog_default(),
        );
        cmd_last.execute(&mut project);
        let after_last = graph_of(&project, &fx);
        assert_eq!(modifier_ids_in_wire_order(after_last, &[1]), vec![ids0[0]]);
        cmd_last.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &before, "undo restores after removing the last");
    }

    #[test]
    fn move_modifier_reorders_stack_and_undo_restores() {
        let (mut project, fx) =
            project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh", "node.taper_mesh"]));
        let before = graph_of(&project, &fx).clone();
        let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist, taper]

        // Move taper (currently last) to position 0 — just after the mesh source.
        let mut cmd = MoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[2],
            0,
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let ids1 = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids1, vec![ids0[2], ids0[0], ids0[1]], "taper moved to the front, bend/twist shift down");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-move graph exactly (inverse-pair)");
    }

    #[test]
    fn move_modifier_to_the_end_and_undo_restores() {
        let (mut project, fx) = project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh"]));
        let before = graph_of(&project, &fx).clone();
        let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist]

        // Move bend (currently first) to the end.
        let mut cmd = MoveMeshModifierCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            ids0[0],
            1, // one slot remains (twist) once bend is detached — "end" is position 1
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        assert_eq!(modifier_ids_in_wire_order(def, &[1]), vec![ids0[1], ids0[0]]);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-move graph exactly (inverse-pair)");
    }
}
