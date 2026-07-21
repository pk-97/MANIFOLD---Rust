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
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, PresetMetadata,
    SerializedParamValue, SkipModeDef,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{stamp_scene_node_exposures_into, SceneParamMetadata};

use crate::command::Command;

mod node_edit;
pub use node_edit::*;
mod expose;
pub use expose::*;
mod groups;
pub use groups::*;
mod scene;
pub use scene::*;

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
pub(super) fn with_target_graph_mut<F, R>(
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
pub(super) fn with_existing_target_graph_mut<F, R>(
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

/// Refresh the target's live `ParamManifest` from its just-mutated graph
/// metadata (BUG-295). `with_target_graph_mut`/`with_existing_target_graph_mut`
/// bump `graph_version`/`graph_structure_version` — a different counter the
/// renderer watches for chain rebuilds — but never touch
/// `PresetInstance::params` itself, so a command that stamps a freshly-minted
/// node's exposures into `preset_metadata.params` (or restores a prior
/// `preset_metadata` on undo) leaves the panel's live manifest stale until a
/// save+reload round trip. Called after every scene-structural command that
/// touches `preset_metadata` at runtime — see call sites below. A no-op if
/// the target no longer resolves (effect/layer deleted).
pub(super) fn refresh_target_manifest(project: &mut Project, target: &GraphTarget) {
    project.with_preset_graph_mut(target, |host| host.refresh_manifest_from_graph());
}

/// Helper for the Revert command: take the target's current
/// `Option<EffectGraphDef>` (consuming it; leaves `None` in place) and
/// return what was there. Bumps the version counter.
pub(super) fn take_target_graph(
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
pub(super) fn install_target_graph(
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


// ---------------------------------------------------------------------------
// Group / Ungroup
// ---------------------------------------------------------------------------

/// Navigate to the node + wire vectors of the sub-graph at `scope` — a list of
/// group-node ids to descend into (empty = the document root). Returns `None`
/// if a hop doesn't resolve to a group. The mutable handles let a command both
/// read the level (snapshot for undo) and replace it (apply the transform).
pub(super) fn descend_level<'a>(
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
pub(super) fn innermost_group_display_name(nodes: &[EffectGraphNode], scope: &[u32]) -> Option<String> {
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
pub(super) fn collect_node_ids(nodes: &[EffectGraphNode], out: &mut Vec<NodeId>) {
    for n in nodes {
        if !n.node_id.is_empty() {
            out.push(n.node_id.clone());
        }
        if let Some(body) = n.group.as_deref() {
            collect_node_ids(&body.nodes, out);
        }
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
pub(super) fn scene_build_node(id: u32, type_id: &str, handle: Option<String>, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
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

pub(super) fn scene_build_wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// Resolve the `&mut PresetInstance` a [`GraphTarget`] addresses — same match
/// every `graph.rs` command uses (mirrors `ToggleNodeParamExposeCommand`'s
/// identical resolve for its mirror step). Used by rename commands' D5 card-
/// section sweep, which needs the manifest (`.params`) alongside the graph —
/// outside `with_target_graph_mut`'s narrower `&mut EffectGraphDef` view.
/// Free function (not a method) so both [`RenameGroupCommand`] and
/// [`RenameSceneObjectCommand`] share one implementation.
pub(super) fn resolve_target_instance<'p>(
    target: &GraphTarget,
    project: &'p mut Project,
) -> Option<&'p mut manifold_core::effects::PresetInstance> {
    match target {
        GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
        GraphTarget::Generator(layer_id) => {
            project.timeline.find_layer_by_id_mut(layer_id).map(|(_, layer)| layer.gen_params_or_init())
        }
    }
}

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
const MESH_MODIFIER_TYPE_IDS: &[&str] = &[
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
pub(super) fn dedup_handle(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
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
    use manifold_core::effect_graph_def::{
        BindingDef, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef,
        StringBindingDef,
    };
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

    /// A generator-hosted twin of [`project_with_graph`] (BUG-295 regression
    /// coverage): production scene commands always target
    /// `GraphTarget::Generator` — `is_generator()` gates
    /// `gather_known_params`'s full-`meta.params`-authority branch, which is
    /// what actually lets a freshly stamped exposure (whose binding carries
    /// `user_added: false`, `scene_exposure.rs`) surface into the live
    /// manifest. An `Effect`-target fixture like `project_with_graph` would
    /// silently take the OTHER `gather_known_params` branch (registry
    /// `param_defs` + `user_added`-flagged bindings only) and never see the
    /// stamped param at all — not a proof of the live-refresh fix.
    fn project_with_generator_graph(def: EffectGraphDef) -> (Project, LayerId) {
        let mut project = Project::default();
        let mut layer = Layer::new("Test Layer".to_string(), LayerType::Generator, 0);
        let lid = layer.layer_id.clone();
        layer.gen_params_or_init().graph = Some(def);
        project.timeline.layers.push(layer);
        (project, lid)
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
            Vec::new(),
            Vec::new(),
            Vec::new(),
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
        assert_eq!(
            body.nodes.len(),
            5,
            "cube + material + transform + scene_object bind + group_output boundary"
        );
        assert!(body.nodes.iter().any(|n| n.type_id == "node.cube_mesh"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.phong_material"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.transform_3d"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.scene_object"));
        assert_eq!(
            body.wires.len(),
            4,
            "mesh/material/transform wired to scene_object, scene_object wired to the group_output"
        );
        assert_eq!(body.interface.outputs.len(), 1, "a single Object output");
        assert_eq!(body.interface.outputs[0].name, "object");
        assert_eq!(body.interface.outputs[0].port_type, "Object");

        // SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3/D4: the group's single
        // `object` output wired to render_scene's new object_2 slot.
        assert!(def.wires.iter().any(|w| w.from_node == group.id
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_2"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }

    /// A single-param `SceneParamMetadata` fixture — stands in for what
    /// `manifold_renderer::node_graph::scene_exposure::metadata_for_node_type`
    /// would compute from a real primitive's `ParamDef` (this crate can't
    /// depend on the renderer, so the app-side caller is the real source —
    /// see the cross-crate constraint note in
    /// `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1).
    fn scene_param_meta(name: &str, label: &str) -> manifold_core::scene_exposure::SceneParamMetadata {
        manifold_core::scene_exposure::SceneParamMetadata {
            name: name.to_string(),
            label: label.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: SerializedParamValue::Float { value: 0.5 },
            is_angle: false,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            convert: manifold_core::effects::ParamConvert::Float,
        }
    }

    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneObjectCommand`
    /// stamps the material/transform/scene_object metadata the caller hands
    /// it into the def's TOP-LEVEL `preset_metadata`, targeting each new
    /// node's bare `NodeId`, with the section named per the convention
    /// (`"{handle} — Material"` / `"{handle} — Transform"` / `handle`).
    /// Undo restores `preset_metadata` verbatim; execute→undo→redo is stable.
    #[test]
    fn add_scene_object_command_stamps_exposures_and_undo_redo_are_stable() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

        let mut cmd = AddSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            vec![scene_param_meta("ambient", "Ambient")],
            vec![scene_param_meta("pos_x", "X")],
            vec![scene_param_meta("visible", "Visible")],
            mirror_catalog_default(),
        );

        // Asserted after both the first execute and the redo: `execute`
        // mints a fresh random NodeId every call (`scene_build_node` ->
        // `manifold_core::short_id()`, pre-existing behavior, not a P1
        // change), so graph IDENTITY isn't byte-stable across redo — only
        // the STRUCTURE the stamping produces is. "Stable" here means the
        // exposures always target whichever node currently sits in that
        // role, not a frozen id.
        let assert_stamped = |project: &Project| {
            let def = graph_of(project, &fx);
            let group = def.nodes.iter().find(|n| n.handle.as_deref() == Some("Object 1")).unwrap();
            let body = group.group.as_deref().unwrap();
            let mat_node = body.nodes.iter().find(|n| n.type_id == "node.phong_material").unwrap();
            let transform_node = body.nodes.iter().find(|n| n.type_id == "node.transform_3d").unwrap();
            let scene_object_node = body.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();

            let meta = def.preset_metadata.as_ref().expect("P1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 3, "one ParamSpecDef per exposed param");
            assert_eq!(meta.bindings.len(), 3);

            let has_binding = |node_id: &NodeId, param: &str, section: &str| {
                meta.bindings.iter().any(|b| {
                    matches!(&b.target, BindingTarget::Node { node_id: nid, param: p } if nid == node_id && p == param)
                }) && meta.params.iter().any(|p| p.section.as_deref() == Some(section))
            };
            assert!(
                has_binding(&mat_node.node_id, "ambient", "Object 1 — Material"),
                "material exposure targets the grouped node's bare NodeId, section 'Object 1 — Material'"
            );
            assert!(
                has_binding(&transform_node.node_id, "pos_x", "Object 1 — Transform"),
                "transform exposure targets the grouped node's bare NodeId, section 'Object 1 — Transform'"
            );
            assert!(
                has_binding(&scene_object_node.node_id, "visible", "Object 1"),
                "scene_object exposure targets the grouped node's bare NodeId, section 'Object 1'"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(def.preset_metadata.is_none(), "undo restores the pre-add (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
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
            Vec::new(),
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

    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneLightCommand`
    /// stamps the caller-supplied light metadata into the def's TOP-LEVEL
    /// `preset_metadata`, targeting the new light's bare `NodeId`, section
    /// "Light N" (1-based display convention, independent of the node's own
    /// internal `light_{k}` handle). Undo restores `preset_metadata`
    /// verbatim; execute→undo→redo is structurally stable (see the
    /// AddSceneObjectCommand sibling test for why redo isn't byte-identical:
    /// `execute` mints a fresh random NodeId every call).
    #[test]
    fn add_scene_light_command_stamps_exposures_and_undo_redo_are_stable() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

        let mut cmd = AddSceneLightCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (-260.0, 50.0),
            vec![scene_param_meta("intensity", "Intensity")],
            mirror_catalog_default(),
        );

        let assert_stamped = |project: &Project| {
            let def = graph_of(project, &fx);
            let light = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap();

            let meta = def.preset_metadata.as_ref().expect("P1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 1);
            assert_eq!(meta.params[0].section.as_deref(), Some("Light 1"));
            assert!(
                meta.bindings.iter().any(|b| matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == light.node_id && param == "intensity"
                )),
                "light exposure targets the light's bare NodeId"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(def.preset_metadata.is_none(), "undo restores the pre-add (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
    }

    // ── BUG-193: Remove Scene Object / Remove Scene Light ──

    /// A fixture with 3 objects wired as `AddSceneObjectCommand` builds them
    /// (group + mesh_k/material_k/transform_k wires), so removal tests can
    /// exercise the middle-object renumbering case (BUG-193's core claim).
    /// Builds `count` bare `node.scene_object` producers wired directly to
    /// `render_scene`'s `object_k` ports (the D3/D4 shape) — hand-built
    /// rather than via `AddSceneObjectCommand`, whose `catalog_default` still
    /// emits the pre-migration legacy-port shape (P3's job to retarget, see
    /// docs/BUG_BACKLOG.md). Returns the def and each producer's node id.
    fn render_scene_with_objects(count: u32) -> (EffectGraphDef, Vec<u32>) {
        let mut def = render_scene_graph(0, 0);
        def.nodes.iter_mut().find(|n| n.id == 0).unwrap().params.insert(
            "objects".to_string(),
            SerializedParamValue::Float { value: count as f32 },
        );
        let mut object_ids = Vec::new();
        for k in 0..count {
            let id = 100 + k;
            def.nodes.push(EffectGraphNode {
                id,
                node_id: manifold_core::NodeId::new(format!("obj{k}")),
                type_id: "node.scene_object".to_string(),
                handle: Some(format!("Object {}", k + 1)),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            });
            def.wires.push(EffectGraphWire {
                from_node: id,
                from_port: "object".to_string(),
                to_node: 0,
                to_port: format!("object_{k}"),
            });
            object_ids.push(id);
        }
        (def, object_ids)
    }

    #[test]
    fn remove_scene_object_middle_deletes_group_and_renumbers_survivors() {
        let (fixture, object_ids) = render_scene_with_objects(3);
        let (mut project, fx) = project_with_graph(fixture);
        let before = graph_of(&project, &fx).clone();

        let mut cmd = RemoveSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            1, // remove the MIDDLE object (index 1 of 0,1,2)
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("objects"),
            Some(&SerializedParamValue::Float { value: 2.0 }),
            "objects decremented by one"
        );
        assert!(
            !def.nodes.iter().any(|n| n.id == object_ids[1]),
            "the removed object's scene_object node is gone"
        );
        assert!(
            def.nodes.iter().any(|n| n.id == object_ids[0]),
            "object 0 survives untouched"
        );
        assert!(
            def.nodes.iter().any(|n| n.id == object_ids[2]),
            "object 2 survives (renumbered)"
        );
        // Object 0 stays at slot 0.
        assert!(def.wires.iter().any(|w| w.from_node == object_ids[0]
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_0"));
        // Object 2 (formerly slot 2) is renumbered down to slot 1.
        assert!(def.wires.iter().any(|w| w.from_node == object_ids[2]
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_1"));
        // No dangling slot-2 wires left behind.
        assert!(!def.wires.iter().any(|w| w.to_node == 0 && w.to_port == "object_2"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-remove graph exactly (inverse-pair)");
    }

    #[test]
    fn remove_scene_light_only_light_removes_node_and_zeroes_count() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 1));
        // Wire the fixture's declared single light exactly like
        // AddSceneLightCommand would (bare node, no group).
        {
            let mut cmd = AddSceneLightCommand::new(
                GraphTarget::Effect(fx.clone()),
                vec![],
                0,
                0,
                (-260.0, 50.0),
                Vec::new(),
                mirror_catalog_default(),
            );
            cmd.execute(&mut project);
        }
        let before = graph_of(&project, &fx).clone();
        let light_id = before
            .nodes
            .iter()
            .find(|n| n.type_id == "node.light")
            .expect("light node present")
            .id;

        let mut cmd = RemoveSceneLightCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("lights"),
            Some(&SerializedParamValue::Float { value: 0.0 }),
            "lights decremented to zero"
        );
        assert!(!def.nodes.iter().any(|n| n.id == light_id), "light node removed");
        assert!(!def.wires.iter().any(|w| w.to_node == 0 && w.to_port == "light_0"), "wire removed");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-remove graph exactly (inverse-pair)");
    }

    // ── SCENE_OBJECT_AND_PANEL_V2_DESIGN P3: Duplicate / Rename ──

    /// Every stable [`NodeId`] and doc `id` anywhere in `nodes`, recursively
    /// through nested groups — test helper mirroring `collect_node_ids` +
    /// `max_node_id_over`, used to prove a duplicate mints fresh identity
    /// throughout its whole cloned subtree, not just the top node.
    fn collect_ids(nodes: &[EffectGraphNode], doc_ids: &mut Vec<u32>, node_ids: &mut Vec<NodeId>) {
        for n in nodes {
            doc_ids.push(n.id);
            if !n.node_id.is_empty() {
                node_ids.push(n.node_id.clone());
            }
            if let Some(body) = n.group.as_deref() {
                collect_ids(&body.nodes, doc_ids, node_ids);
            }
        }
    }

    #[test]
    fn duplicate_scene_object_command_clones_grouped_object_with_fresh_ids_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        AddSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            mirror_catalog_default(),
        )
        .execute(&mut project);
        let before = graph_of(&project, &fx).clone();
        let (mut orig_doc_ids, mut orig_node_ids) = (Vec::new(), Vec::new());
        collect_ids(&before.nodes, &mut orig_doc_ids, &mut orig_node_ids);

        let mut cmd = DuplicateSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0, // duplicate object 0 (the only object)
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("objects"),
            Some(&SerializedParamValue::Float { value: 2.0 }),
            "objects bumped by one"
        );
        let clone = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("Object 1 2"))
            .expect("clone named with the D11 ' 2' suffix");
        assert!(def.wires.iter().any(|w| w.from_node == clone.id
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_1"), "clone wired to the next free object slot");

        // D11: every id in the clone's subtree is fresh — no overlap with
        // the original's doc ids or stable NodeIds anywhere.
        let (mut all_doc_ids, mut all_node_ids) = (Vec::new(), Vec::new());
        collect_ids(&def.nodes, &mut all_doc_ids, &mut all_node_ids);
        let mut clone_doc_ids = Vec::new();
        let mut clone_node_ids = Vec::new();
        collect_ids(std::slice::from_ref(clone), &mut clone_doc_ids, &mut clone_node_ids);
        for id in &clone_doc_ids {
            assert!(!orig_doc_ids.contains(id), "clone doc id {id} must not reuse an original doc id");
        }
        for nid in &clone_node_ids {
            assert!(!orig_node_ids.contains(nid), "clone NodeId {nid:?} must not reuse an original NodeId");
        }
        // No duplicate doc ids anywhere in the whole def (fresh minting is
        // globally unique, not just locally).
        let mut sorted = all_doc_ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all_doc_ids.len(), "no doc id collisions anywhere in the def");

        // No duplicate handles among SIBLINGS at any one scope — the real
        // constraint the flattener's group-name-prefixing composite naming
        // needs (`Graph::add_node_named` builds on the flattened, prefixed
        // names; two DIFFERENT groups' identically-named inner leaves don't
        // collide because the group name prefixes them, but two nodes in
        // the SAME scope sharing a handle do). The clone's own group got a
        // distinct top handle ("Object 1 2" vs the source's "Object 1"), so
        // this must hold recursively through both subtrees.
        fn assert_no_sibling_handle_collisions(nodes: &[EffectGraphNode]) {
            let mut seen = std::collections::HashSet::new();
            for n in nodes {
                if let Some(h) = &n.handle {
                    assert!(seen.insert(h.clone()), "sibling handle collision at this scope: {h:?}");
                }
                if let Some(body) = n.group.as_deref() {
                    assert_no_sibling_handle_collisions(&body.nodes);
                }
            }
        }
        assert_no_sibling_handle_collisions(&def.nodes);

        // D6: the clone's inner scene_object handle stays in sync with the
        // group's handle.
        let clone_body = clone.group.as_deref().expect("clone is a group");
        let inner_object = clone_body.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
        assert_eq!(inner_object.handle.as_deref(), Some("Object 1 2"));

        // D11: transform_3d.pos_x offset by +0.5 on the clone.
        let clone_transform = clone_body.nodes.iter().find(|n| n.type_id == "node.transform_3d").unwrap();
        assert_eq!(clone_transform.params.get("pos_x"), Some(&SerializedParamValue::Float { value: 0.5 }));

        // D11: card exposes are not cloned.
        assert!(clone_body.nodes.iter().all(|n| n.exposed_params.is_empty()));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-duplicate graph exactly (inverse-pair)");
    }

    /// BUG-212: `string_bindings` (the importer's "Model File" path
    /// plumbing — one `StringBindingDef` per file-dependent node, fanned
    /// out under a shared outer id) must follow a duplicated object's
    /// cloned nodes, re-targeted at the clone's fresh `NodeId`, same
    /// `id`/`label`/`default_value` — the same mechanism as D5's rename
    /// sweep, exercised here for `DuplicateSceneObjectCommand`.
    #[test]
    fn duplicate_scene_object_command_clones_string_bindings_onto_fresh_node_id_and_undo_restores() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        AddSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            mirror_catalog_default(),
        )
        .execute(&mut project);

        // Simulate the importer's "Model File" binding: one string_bindings
        // entry targeting the object's mesh node by its stable NodeId.
        let mesh_node_id = {
            let def = graph_of(&project, &fx);
            let group = def.nodes.iter().find(|n| n.handle.as_deref() == Some("Object 1")).unwrap();
            let mesh = group.group.as_ref().unwrap().nodes.iter().find(|n| n.type_id == "node.cube_mesh").unwrap();
            mesh.node_id.clone()
        };
        {
            let effect = project.find_effect_by_id_mut(&fx).unwrap();
            let def = effect.graph.as_mut().unwrap();
            def.preset_metadata = Some(PresetMetadata {
                id: PresetTypeId::new("test.scene"),
                display_name: "Test Scene".into(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: Vec::new(),
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: vec![StringBindingDef {
                    id: "model_file".into(),
                    label: "Model File".into(),
                    default_value: "assets/hero.glb".into(),
                    target: BindingTarget::Node { node_id: mesh_node_id.clone(), param: "path".into() },
                }],
            });
        }
        let before_meta = graph_of(&project, &fx).preset_metadata.clone().unwrap();

        let mut cmd = DuplicateSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0, // duplicate object 0 (the only object)
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let clone = def.nodes.iter().find(|n| n.handle.as_deref() == Some("Object 1 2")).unwrap();
        let clone_mesh = clone.group.as_ref().unwrap().nodes.iter().find(|n| n.type_id == "node.cube_mesh").unwrap();

        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.string_bindings.len(), 2, "the clone's mesh node gets its own string_bindings entry");
        let clone_binding = meta
            .string_bindings
            .iter()
            .find(|b| matches!(&b.target, BindingTarget::Node { node_id, .. } if *node_id == clone_mesh.node_id))
            .expect("a string_bindings entry targets the clone's fresh NodeId");
        assert_eq!(clone_binding.id, "model_file");
        assert_eq!(clone_binding.default_value, "assets/hero.glb", "same default_value as the source entry");
        // The original entry (still targeting the SOURCE mesh's NodeId) is untouched.
        assert!(meta.string_bindings.iter().any(
            |b| matches!(&b.target, BindingTarget::Node { node_id, .. } if *node_id == mesh_node_id)
        ));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(
            def.preset_metadata.as_ref().unwrap(),
            &before_meta,
            "undo restores string_bindings exactly (inverse-pair)"
        );
    }

    #[test]
    fn rename_scene_object_command_renames_group_and_sweeps_section_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        AddSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            mirror_catalog_default(),
        )
        .execute(&mut project);
        let def = graph_of(&project, &fx).clone();
        let group = def.nodes.iter().find(|n| n.handle.as_deref() == Some("Object 1")).unwrap();
        let group_id = group.id;
        let mat_node = group.group.as_ref().unwrap().nodes.iter().find(|n| n.type_id == "node.phong_material").unwrap();
        let (mat_node_id, mat_u32_id) = (mat_node.node_id.clone(), mat_node.id);

        ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            mat_node_id,
            mat_u32_id,
            "mat_0".to_string(),
            "ambient".to_string(),
            true,
            mirror_catalog_default(),
            "Ambient".to_string(),
            0.0,
            1.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        )
        .with_scope(vec![group_id])
        .execute(&mut project);
        let ub_id = project.find_effect_by_id(&fx).unwrap().user_param_bindings()[0].id.clone();
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("Object 1"),
            "setup: expose seeded the section from the group name"
        );

        let before = graph_of(&project, &fx).clone();
        let mut cmd = RenameSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            group_id,
            "Hero".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let group = def.nodes.iter().find(|n| n.id == group_id).unwrap();
        assert_eq!(group.handle.as_deref(), Some("Hero"), "group handle renamed");
        let inner_object =
            group.group.as_ref().unwrap().nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
        assert_eq!(inner_object.handle.as_deref(), Some("Hero"), "scene_object handle kept in sync (D6)");
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("Hero"),
            "D5: card section follows the rename"
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-rename graph exactly (inverse-pair)");
        assert_eq!(
            project.find_effect_by_id(&fx).unwrap().params.get(&ub_id).unwrap().spec.section.as_deref(),
            Some("Object 1"),
            "undo restores the pre-rename section"
        );
    }

    #[test]
    fn rename_scene_object_command_ungrouped_renames_bare_node_and_undo_restores() {
        let (fixture, object_ids) = render_scene_with_objects(2);
        let (mut project, fx) = project_with_graph(fixture);
        let before = graph_of(&project, &fx).clone();

        let mut cmd = RenameSceneObjectCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            object_ids[0],
            "Renamed".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let node = def.nodes.iter().find(|n| n.id == object_ids[0]).unwrap();
        assert_eq!(node.handle.as_deref(), Some("Renamed"));
        assert!(node.group.is_none(), "ungrouped node stays bare, no group is fabricated");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-rename graph exactly (inverse-pair)");
    }

    #[test]
    fn set_node_handle_command_renames_light_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
        AddSceneLightCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            Vec::new(),
            mirror_catalog_default(),
        )
        .execute(&mut project);
        let before = graph_of(&project, &fx).clone();
        let light_id = before.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;

        let mut cmd = SetNodeHandleCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            light_id,
            "Key Light".to_string(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        assert_eq!(
            def.nodes.iter().find(|n| n.id == light_id).unwrap().handle.as_deref(),
            Some("Key Light")
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-rename graph exactly (inverse-pair)");
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
            Vec::new(),
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
            Vec::new(),
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

    /// R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneEnvironmentCommand`
    /// stamps the caller-supplied environment metadata into the def's
    /// TOP-LEVEL `preset_metadata`, targeting the new environment node's bare
    /// `NodeId`, section "Environment" — same P1 stamp shape
    /// `AddSceneLightCommand` performs for its own node. Regression coverage
    /// for the R1 bug: a freshly-added environment was structurally invisible
    /// in the scene panel because `world_sections` (`state_sync.rs`'s
    /// `sections_for_doc_ids`) came back empty with nothing stamped. Undo
    /// restores `preset_metadata` verbatim; execute→undo→redo is stable.
    #[test]
    fn add_scene_environment_command_stamps_exposures_and_undo_redo_are_stable() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

        let mut cmd = AddSceneEnvironmentCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            (10.0, 20.0),
            vec![scene_param_meta("intensity", "Intensity")],
            mirror_catalog_default(),
        );

        let assert_stamped = |project: &Project| {
            let def = graph_of(project, &fx);
            let env = def.nodes.iter().find(|n| n.type_id == "node.bake_environment").unwrap();

            let meta = def.preset_metadata.as_ref().expect("R1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 1);
            assert_eq!(meta.params[0].section.as_deref(), Some("Environment"));
            assert!(
                meta.bindings.iter().any(|b| matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == env.node_id && param == "intensity"
                )),
                "environment exposure targets the environment node's bare NodeId"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(def.preset_metadata.is_none(), "undo restores the pre-add (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
    }

    /// R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneFogCommand`
    /// stamps the caller-supplied fog metadata into the def's TOP-LEVEL
    /// `preset_metadata`, targeting the new fog node's bare `NodeId`, section
    /// "Atmosphere" — same P1 stamp shape `AddSceneLightCommand` performs for
    /// its own node. Regression coverage for the R1 bug this lane fixes: a
    /// freshly-added fog node was structurally invisible in the scene panel
    /// (not even the fallback row rendered) because `world_sections`
    /// (`state_sync.rs`'s `sections_for_doc_ids`) came back empty with
    /// nothing stamped, and `build_filtered_properties` iterates an empty
    /// section list. Undo restores `preset_metadata` verbatim; execute→undo→
    /// redo is stable.
    #[test]
    fn add_scene_fog_command_stamps_exposures_and_undo_redo_are_stable() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

        let mut cmd = AddSceneFogCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            (30.0, 40.0),
            vec![scene_param_meta("density", "Density")],
            mirror_catalog_default(),
        );

        let assert_stamped = |project: &Project| {
            let def = graph_of(project, &fx);
            let fog = def.nodes.iter().find(|n| n.type_id == "node.atmosphere").unwrap();

            let meta = def.preset_metadata.as_ref().expect("R1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 1);
            assert_eq!(meta.params[0].section.as_deref(), Some("Atmosphere"));
            assert!(
                meta.bindings.iter().any(|b| matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == fog.node_id && param == "density"
                )),
                "fog exposure targets the fog node's bare NodeId"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert!(def.preset_metadata.is_none(), "undo restores the pre-add (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
    }

    /// BUG-295: `AddSceneFogCommand` stamps the fog exposure into
    /// `def.preset_metadata.params` (proven above), but until
    /// `refresh_manifest_from_graph` is ALSO wired to run post-stamp, that
    /// stamp is invisible to the LIVE `PresetInstance.params` the panel
    /// actually reads — the bug's own root-cause finding (`reconcile_manifest`
    /// only fires from a load-time `pending_wire` stash, never from a runtime
    /// graph edit). Regression coverage for the live-manifest half of the fix:
    /// execute → the fog row is in `inst.params`, not just `preset_metadata`;
    /// undo → the row is gone from `inst.params`; redo → it's back. Targets
    /// `GraphTarget::Generator` (see `project_with_generator_graph`) so
    /// `gather_known_params`'s generator branch actually picks up the
    /// stamped `meta.params` entry regardless of the binding's `user_added`
    /// flag (scene exposures are always `user_added: false`).
    #[test]
    fn add_scene_fog_command_refreshes_live_manifest_and_undo_redo_restore_it() {
        let (mut project, lid) = project_with_generator_graph(render_scene_graph(0, 0));

        let mut cmd = AddSceneFogCommand::new(
            GraphTarget::Generator(lid.clone()),
            vec![],
            0,
            (30.0, 40.0),
            vec![scene_param_meta("density", "Density")],
            mirror_catalog_default(),
        );

        let has_fog_row = |project: &Project| {
            project
                .timeline
                .find_layer_by_id(&lid)
                .unwrap()
                .1
                .gen_params()
                .unwrap()
                .params
                .iter()
                .any(|p| p.spec.section.as_deref() == Some("Atmosphere"))
        };

        cmd.execute(&mut project);
        assert!(
            has_fog_row(&project),
            "BUG-295: freshly-stamped fog param must land in the live inst.params after execute"
        );

        cmd.undo(&mut project);
        assert!(
            !has_fog_row(&project),
            "undo must remove the fog row from the live manifest, not just def.preset_metadata"
        );

        cmd.execute(&mut project); // redo
        assert!(has_fog_row(&project), "redo must restore the live fog row");
    }

    /// BUG-295 value-preservation proof: `refresh_manifest_from_graph`
    /// round-trips the CURRENT manifest through the same wire encoding the
    /// file serializer uses before overlaying the graph's descriptors, so a
    /// pre-existing param's live (possibly non-default) value must survive a
    /// LATER structural edit's refresh — not just the freshly-stamped one's
    /// own default. Sets a light's Intensity to a hand-picked non-default
    /// value, then executes `AddSceneFogCommand` (a second, unrelated
    /// structural edit) and asserts Intensity kept its value rather than
    /// resetting to the spec default a naive `build_param_manifest(..., None)`
    /// resync would have produced.
    #[test]
    fn add_scene_fog_command_refresh_preserves_existing_param_values() {
        let (mut project, lid) = project_with_generator_graph(render_scene_graph(0, 0));

        let mut add_light = AddSceneLightCommand::new(
            GraphTarget::Generator(lid.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            vec![scene_param_meta("intensity", "Intensity")],
            mirror_catalog_default(),
        );
        add_light.execute(&mut project);

        let intensity_id = project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .params
            .iter()
            .find(|p| p.spec.name == "Intensity")
            .expect("add-light's own refresh surfaced the stamped Intensity param live")
            .id()
            .to_string();

        project
            .timeline
            .find_layer_by_id_mut(&lid)
            .unwrap()
            .1
            .gen_params_or_init()
            .params
            .get_mut(&intensity_id)
            .expect("intensity param resolves by its synthesized id")
            .value = 0.42;

        let mut add_fog = AddSceneFogCommand::new(
            GraphTarget::Generator(lid.clone()),
            vec![],
            0,
            (30.0, 40.0),
            vec![scene_param_meta("density", "Density")],
            mirror_catalog_default(),
        );
        add_fog.execute(&mut project);

        let intensity_value = project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .params
            .get(&intensity_id)
            .expect("intensity param survives the fog add's refresh")
            .value;
        assert_eq!(
            intensity_value, 0.42,
            "BUG-295 refresh must preserve a pre-existing param's live value, not reset it to spec default"
        );
    }

    // ── REALTIME_3D_DESIGN P6: Add Object Transform (gizmo auto-create) ──

    fn scene_object_graph() -> EffectGraphDef {
        let render = EffectGraphNode {
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
        let object = EffectGraphNode {
            id: 1,
            node_id: manifold_core::NodeId::new("obj"),
            type_id: "node.scene_object".to_string(),
            handle: Some("Statue".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, object],
            wires: vec![EffectGraphWire {
                from_node: 1,
                from_port: "object".to_string(),
                to_node: 0,
                to_port: "object_0".to_string(),
            }],
        }
    }

    #[test]
    fn add_object_transform_command_spawns_transform_3d_and_wires_it_into_scene_object() {
        let (mut project, fx) = project_with_graph(scene_object_graph());
        let before = graph_of(&project, &fx).clone();

        let mut cmd = AddObjectTransformCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            (5.0, 6.0),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);
        let xf_id = cmd.created_node_id().expect("command should resolve and create a node");

        let def = graph_of(&project, &fx);
        let xf = def.nodes.iter().find(|n| n.id == xf_id).expect("transform node exists");
        assert_eq!(xf.type_id, "node.transform_3d");
        assert_eq!(xf.editor_pos, Some((5.0, 6.0)));
        assert!(def
            .wires
            .iter()
            .any(|w| w.from_node == xf_id && w.from_port == "transform" && w.to_node == 1 && w.to_port == "transform"));

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-add graph exactly (inverse-pair)");
    }

    #[test]
    fn add_object_transform_then_gizmo_param_drag_round_trips_undo_redo() {
        let (mut project, fx) = project_with_graph(scene_object_graph());
        let before = graph_of(&project, &fx).clone();

        let mut add_cmd = AddObjectTransformCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            1,
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        add_cmd.execute(&mut project);
        let xf_id = add_cmd.created_node_id().unwrap();
        let after_create = graph_of(&project, &fx).clone();

        // The gizmo's first move-axis drag: write pos_x on the freshly
        // created transform atom (D8's drag-writes-the-transform-atom path).
        let mut set_cmd = SetGraphNodeParamCommand::new(
            GraphTarget::Effect(fx.clone()),
            xf_id,
            "pos_x".to_string(),
            SerializedParamValue::Float { value: 3.5 },
            mirror_catalog_default(),
        );
        set_cmd.execute(&mut project);
        let def = graph_of(&project, &fx);
        let xf = def.nodes.iter().find(|n| n.id == xf_id).unwrap();
        assert_eq!(xf.params.get("pos_x"), Some(&SerializedParamValue::Float { value: 3.5 }));

        // Undo the drag: pos_x reverts (the transform atom itself, and its
        // wire, stay — same as any other param undo).
        set_cmd.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &after_create, "undo of the drag restores pre-drag graph");

        // Redo the drag.
        set_cmd.execute(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(
            def.nodes.iter().find(|n| n.id == xf_id).unwrap().params.get("pos_x"),
            Some(&SerializedParamValue::Float { value: 3.5 })
        );

        // Undo the drag AND the atom creation: back to the original,
        // transform-less graph — the full round trip P6's gate names.
        set_cmd.undo(&mut project);
        add_cmd.undo(&mut project);
        assert_eq!(graph_of(&project, &fx), &before, "full undo restores the original graph");
    }

    // ── SCENE_SETUP_PANEL_DESIGN P4: Import Model into Scene (merge) ──

    /// A plain, un-grouped merged object node (mesh source + material +
    /// transform, no group wrapper) — this test exercises the COMMAND, not
    /// the assembler, so a minimal top-level node stands in for the
    /// (grouped) shape `merge_import_into_graph` would actually produce.
    fn plain_merge_node(id: u32, handle: &str, type_id: &str) -> EffectGraphNode {
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

    #[test]
    fn import_model_into_scene_command_bumps_objects_adds_nodes_wires_and_undo_restores() {
        let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
        let before = graph_of(&project, &fx).clone();

        let new_node = plain_merge_node(100, "MergedObject", GROUP_TYPE_ID);
        let new_wire = EffectGraphWire {
            from_node: 100,
            from_port: "vertices".to_string(),
            to_node: 0,
            to_port: "mesh_2".to_string(),
        };

        let mut cmd = ImportModelIntoSceneCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            vec![new_node],
            vec![new_wire],
            3,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("objects"),
            Some(&SerializedParamValue::Float { value: 3.0 }),
            "objects bumped to existing(2) + incoming(1)"
        );
        assert!(
            def.nodes.iter().any(|n| n.id == 100 && n.handle.as_deref() == Some("MergedObject")),
            "the merged node must be present"
        );
        assert!(
            def.wires.iter().any(|w| w.from_node == 100 && w.to_node == 0 && w.to_port == "mesh_2"),
            "the merged wire must be present"
        );

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-merge graph exactly (inverse-pair)");
    }

    #[test]
    fn import_model_into_scene_command_extends_card_metadata_and_undo_restores() {
        let mut base = render_scene_graph(1, 0);
        base.preset_metadata = Some(PresetMetadata {
            id: manifold_core::PresetTypeId::from_string("Existing".to_string()),
            display_name: "Existing".to_string(),
            category: "Geometry".to_string(),
            osc_prefix: "existing".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![],
            bindings: vec![],
            skip_mode: SkipModeDef::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        let (mut project, fx) = project_with_graph(base);
        let before = graph_of(&project, &fx).clone();

        let new_param = ParamSpecDef {
            id: "opacity_1".to_string(),
            name: "Opacity".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 1.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: Some("MergedGlass".to_string()),
        };
        let new_binding = BindingDef {
            id: "opacity_1".to_string(),
            label: "Opacity".to_string(),
            default_value: 1.0,
            target: manifold_core::effect_graph_def::BindingTarget::Node {
                node_id: manifold_core::NodeId::new("mat_1"),
                param: "color_a".to_string(),
            },
            convert: manifold_core::effects::ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        };

        let mut cmd = ImportModelIntoSceneCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            vec![plain_merge_node(50, "MergedGlass", GROUP_TYPE_ID)],
            vec![EffectGraphWire {
                from_node: 50,
                from_port: "vertices".to_string(),
                to_node: 0,
                to_port: "mesh_1".to_string(),
            }],
            2,
            vec![new_param],
            vec![new_binding],
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = graph_of(&project, &fx);
        let meta = def.preset_metadata.as_ref().expect("metadata still present");
        assert!(meta.params.iter().any(|p| p.id == "opacity_1"), "new card param appended");
        assert!(meta.bindings.iter().any(|b| b.id == "opacity_1"), "new card binding appended");

        cmd.undo(&mut project);
        let def = graph_of(&project, &fx);
        assert_eq!(def, &before, "undo restores the pre-merge graph AND metadata exactly");
    }

    // ── SCENE_SETUP_PANEL_DESIGN P5: mesh-modifier stack ──

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
