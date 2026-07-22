//! Scene-build commands — add/remove/duplicate scene objects, lights,
//! environment, fog; object transforms; model import; rename; set-handle.
//! Split out of `graph.rs` in P2-G/S5 (pure move). The shared graph helpers
//! (target-graph access, descend_level, collect_node_ids, resolve_target_instance,
//! refresh_target_manifest) and the scene builders `scene_build_node`/
//! `scene_build_wire` (also used by the modifier/paste regions) stay in
//! `graph/mod.rs` and are reached via `super`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef, PresetMetadata,
    SerializedParamValue, SkipModeDef, StringBindingDef,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{stamp_scene_node_exposures_into, SceneParamMetadata};

use crate::command::Command;

use super::{
    collect_node_ids, dedup_handle, descend_level, refresh_target_manifest,
    resolve_target_instance, scene_build_node, scene_build_wire, with_existing_target_graph_mut,
    with_target_graph_mut,
};

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
    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new material/
    /// transform/scene_object nodes' full param manifests, computed by the
    /// app-side caller via `manifold_renderer::node_graph::scene_exposure::
    /// metadata_for_node_type` (this crate has no renderer dep) — `execute`
    /// stamps them into the def's top-level `preset_metadata` after minting
    /// the new nodes' ids.
    material_metadata: Vec<SceneParamMetadata>,
    transform_metadata: Vec<SceneParamMetadata>,
    scene_object_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata` (P1 exposure stamping lands there, outside
    /// the scoped level). Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl AddSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        centroid: (f32, f32),
        material_metadata: Vec<SceneParamMetadata>,
        transform_metadata: Vec<SceneParamMetadata>,
        scene_object_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            next_index,
            centroid,
            material_metadata,
            transform_metadata,
            scene_object_metadata,
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
            let prev_metadata = def.preset_metadata.clone();

            // Build the group + wire it in, entirely within a nested block so
            // the `nodes`/`wires` borrows (from `descend_level`) end before
            // the P1 exposure stamping below touches `def.preset_metadata` —
            // same "metadata vs. nodes/wires never overlap" discipline
            // `ImportModelIntoSceneCommand` documents.
            let (mat_id, mat_node_id, mat_node_params, transform_id, transform_node_id, scene_object_id, scene_object_node_id, handle, prev) = {
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
                let scene_object_id = fresh();
                let out_id = fresh();
                let group_id = fresh();

                let tint = scene_object_tint(k);
                let mut mat_params = BTreeMap::new();
                mat_params.insert("color_r".to_string(), SerializedParamValue::Float { value: tint.r });
                mat_params.insert("color_g".to_string(), SerializedParamValue::Float { value: tint.g });
                mat_params.insert("color_b".to_string(), SerializedParamValue::Float { value: tint.b });

                let mesh_node = scene_build_node(mesh_id, "node.cube_mesh", Some(format!("mesh_{k}")), BTreeMap::new());
                let mat_node = scene_build_node(mat_id, "node.phong_material", Some(format!("mat_{k}")), mat_params);
                let mat_node_id = mat_node.node_id.clone();
                let mat_node_params = mat_node.params.clone();
                let transform_node = scene_build_node(
                    transform_id,
                    "node.transform_3d",
                    Some(format!("transform_{k}")),
                    BTreeMap::new(),
                );
                let transform_node_id = transform_node.node_id.clone();
                // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D3/P3: binds the mesh/
                // material/transform triple into a single Object wire —
                // handle-stamped so the outliner shows this object's own name,
                // not a producer's. render_scene v2 (D4) has no mesh_k/
                // material_k/transform_k ports any more; it takes object_k only.
                let handle = format!("Object {}", k + 1);
                let scene_object_node =
                    scene_build_node(scene_object_id, "node.scene_object", Some(handle.clone()), BTreeMap::new());
                let scene_object_node_id = scene_object_node.node_id.clone();
                let out_node = scene_build_node(out_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());

                let group_wires = vec![
                    scene_build_wire(mesh_id, "vertices", scene_object_id, "vertices"),
                    scene_build_wire(mat_id, "out", scene_object_id, "material"),
                    scene_build_wire(transform_id, "transform", scene_object_id, "transform"),
                    scene_build_wire(scene_object_id, "object", out_id, "object"),
                ];

                let mut group_node =
                    scene_build_node(group_id, GROUP_TYPE_ID, Some(handle.clone()), BTreeMap::new());
                group_node.editor_pos = Some(centroid);
                group_node.group = Some(Box::new(GroupDef {
                    interface: GroupInterface {
                        inputs: Vec::new(),
                        outputs: vec![InterfacePortDef {
                            name: "object".to_string(),
                            port_type: "Object".to_string(),
                        }],
                        params: Vec::new(),
                    },
                    nodes: vec![mesh_node, mat_node, transform_node, scene_object_node, out_node],
                    wires: group_wires,
                    tint: Some([tint.r, tint.g, tint.b, 1.0]),
                }));

                nodes.push(group_node);
                wires.push(scene_build_wire(group_id, "object", render_id, &format!("object_{k}")));

                (
                    mat_id,
                    mat_node_id,
                    mat_node_params,
                    transform_id,
                    transform_node_id,
                    scene_object_id,
                    scene_object_node_id,
                    handle,
                    prev,
                )
            };

            // P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): expose every
            // param of the freshly minted material/transform/scene_object
            // nodes, into the def's TOP-LEVEL preset_metadata, targeting each
            // node's bare NodeId — same convention the glTF importer uses.
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
                mat_id,
                &mat_node_id,
                "node.phong_material",
                &format!("{handle} — Material"),
                &self.material_metadata,
                &mat_node_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                transform_id,
                &transform_node_id,
                "node.transform_3d",
                &format!("{handle} — Transform"),
                &self.transform_metadata,
                &BTreeMap::new(),
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                scene_object_id,
                &scene_object_node_id,
                "node.scene_object",
                &handle,
                &self.scene_object_metadata,
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
        let scope = self.scope_path.clone();
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
    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new light's full
    /// param manifest, computed by the app-side caller via
    /// `manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.light")`
    /// (this crate has no renderer dep).
    light_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata`. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl AddSceneLightCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        pos: (f32, f32),
        light_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            next_index,
            pos,
            light_metadata,
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
            let prev_metadata = def.preset_metadata.clone();

            let (light_id, light_node_id, light_node_params, prev) = {
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
                let light_node_id = light_node.node_id.clone();
                let light_node_params = light_node.params.clone();
                nodes.push(light_node);
                wires.push(scene_build_wire(light_id, "out", render_id, &format!("light_{k}")));

                (light_id, light_node_id, light_node_params, prev)
            };

            // P1: expose every param of the freshly minted light node, into
            // the def's TOP-LEVEL preset_metadata, targeting its bare NodeId.
            // Section mirrors the D7a display convention ("Light N", 1-based)
            // — independent of the node's own internal `handle` (`light_{k}`,
            // 0-based, used only for wire/lookup bookkeeping).
            let section = format!("Light {}", k + 1);
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
                light_id,
                &light_node_id,
                "node.light",
                &section,
                &self.light_metadata,
                &light_node_params,
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
        let scope = self.scope_path.clone();
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
        "Add Light"
    }
}

// ---------------------------------------------------------------------------
// Remove Scene Object / Remove Scene Light (BUG-193)
// ---------------------------------------------------------------------------

/// Shift every wire into `to_node` whose `to_port` is `{prefix}_{j}` for
/// `j > removed_index` down by one (`{prefix}_{j-1}`) — the renumbering half
/// of a scene-object/light removal, so the surviving slots stay a dense
/// `0..objects`/`0..lights` run with no gap left by the removed index.
fn shift_indexed_ports_down(wires: &mut [EffectGraphWire], to_node: u32, prefix: &str, removed_index: u32) {
    let needle = format!("{prefix}_");
    for w in wires.iter_mut() {
        if w.to_node != to_node {
            continue;
        }
        if let Some(idx_str) = w.to_port.strip_prefix(&needle)
            && let Ok(idx) = idx_str.parse::<u32>()
            && idx > removed_index
        {
            w.to_port = format!("{prefix}_{}", idx - 1);
        }
    }
}

/// The remove-object gesture (BUG-193, retargeted to the SCENE_OBJECT_AND_PANEL_V2
/// `Object` wire model — the object's mesh/transform/material/maps no longer
/// reach `render_scene` as a parallel-port triplet, they arrive as one
/// `object_k` wire out of a `node.scene_object` node, D1/D4): the inverse of
/// [`AddSceneObjectCommand`] — one undoable composite edit that (1) deletes
/// the object's producer node (the `scene_object`'s enclosing group when one
/// exists — the importer/grouped shape, D5 — else the `scene_object` node
/// itself) and its `object_k` wire into `render_scene`, (2) decrements
/// `objects`, (3) renumbers every `object_j` wire (`j > k`) down by one so
/// the slots stay dense. Same whole-level snapshot/restore undo shape as
/// `AddSceneObjectCommand` — a structural composite edit, not a hand-reversed
/// sequence of sub-steps. Ungrouped hand-built objects (a loose `scene_object`
/// whose mesh/transform/material producers are NOT wrapped in a group) are a
/// known gap shared with the pre-migration version of this command — deleting
/// only the `scene_object` node leaves those loose producers orphaned rather
/// than walking the full exclusive-upstream-subgraph D11 describes; tracked
/// for P3 to handle if a real ungrouped scene needs it.
///
/// `object_index` (`k`, the 0-based slot in `object_k`) is resolved by the
/// caller from the live Vm's own `ObjectKnownRow::index` — not re-derived
/// here, same "UI's already-resolved index is the one source of truth"
/// posture `AddSceneObjectCommand::next_index` documents.
#[derive(Debug)]
pub struct RemoveSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    object_index: u32,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl RemoveSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        object_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            object_index,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for RemoveSceneObjectCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.object_index;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let object_port = format!("object_{k}");
            let producer_id = wires
                .iter()
                .find(|w| w.to_node == render_id && w.to_port == object_port)
                .map(|w| w.from_node)?;

            let current_objects = match nodes.iter().find(|n| n.id == render_id)?.params.get("objects") {
                Some(SerializedParamValue::Float { value }) => *value,
                _ => return None,
            };

            nodes.retain(|n| n.id != producer_id);
            wires.retain(|w| !(w.to_node == render_id && w.to_port == object_port));
            shift_indexed_ports_down(wires, render_id, "object", k);

            nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                "objects".to_string(),
                SerializedParamValue::Float {
                    value: (current_objects - 1.0).max(0.0),
                },
            );

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
        "Remove Object"
    }
}

/// The remove-light gesture (BUG-193): the inverse of
/// [`AddSceneLightCommand`] — one undoable composite edit that (1) deletes
/// the bare light node and its single `light_k` wire, (2) decrements
/// `lights`, (3) renumbers every `light_j` (`j > k`) wire down by one. Same
/// whole-level snapshot/restore undo shape as `RemoveSceneObjectCommand`, but
/// single-port (no triplet) since a light is a bare node, not a group.
#[derive(Debug)]
pub struct RemoveSceneLightCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    light_index: u32,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl RemoveSceneLightCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        light_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            light_index,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for RemoveSceneLightCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.light_index;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let light_port = format!("light_{k}");
            let light_id = wires
                .iter()
                .find(|w| w.to_node == render_id && w.to_port == light_port)
                .map(|w| w.from_node)?;

            let current_lights = match nodes.iter().find(|n| n.id == render_id)?.params.get("lights") {
                Some(SerializedParamValue::Float { value }) => *value,
                _ => return None,
            };

            nodes.retain(|n| n.id != light_id);
            wires.retain(|w| !(w.to_node == render_id && w.to_port == format!("light_{k}")));
            shift_indexed_ports_down(wires, render_id, "light", k);

            nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                "lights".to_string(),
                SerializedParamValue::Float {
                    value: (current_lights - 1.0).max(0.0),
                },
            );

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
        "Remove Light"
    }
}

// ---------------------------------------------------------------------------
// Duplicate Scene Object / Rename Scene Object / Rename Light
// (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D11 / D6, P3)
// ---------------------------------------------------------------------------

/// The highest node `id` anywhere in `nodes`, recursively including every
/// nested group body — ids are unique across the WHOLE document (same fact
/// `scene_object_migration.rs`'s `max_node_id_recursive` documents), so a
/// fresh mint must clear every scope's max, not just the scope being minted
/// into. `0` (not `u32::MAX`) for an empty tree — callers add 1 to get the
/// next free id, matching every other fresh-id convention in this module.
fn max_node_id_over(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_over(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

/// Every populated `handle` anywhere in `nodes`, recursively through nested
/// group bodies — `Graph::add_node_named` enforces handle uniqueness across
/// the WHOLE graph (not just one scope: a clone's inner `mesh_0` collides
/// with the ORIGINAL's `mesh_0` even though they live in different group
/// bodies), so the dedup seed for a deep clone must be collected from the
/// entire def, not just the level being edited. Mirrors `collect_node_ids`'s
/// walk, for handles instead of stable NodeIds.
fn collect_all_handles(nodes: &[EffectGraphNode], out: &mut std::collections::HashSet<String>) {
    for n in nodes {
        if let Some(h) = &n.handle {
            out.insert(h.clone());
        }
        if let Some(body) = n.group.as_deref() {
            collect_all_handles(&body.nodes, out);
        }
    }
}

/// Deep-clone `src` (and, recursively, its ENTIRE `group` subtree when it has
/// one) with a FRESH doc `id`, a FRESH stable [`NodeId`], and a deduped
/// `handle` on every node — D11: "bindings are identity, never cloned; fresh
/// NodeIds make cloned bindings dangle by construction" (a stale NodeId on
/// the clone would let a card binding silently double-drive both the
/// original and the copy). Handle dedup (via [`dedup_handle`], the same
/// convention `PasteNodesCommand` uses) is load-bearing, not cosmetic: the
/// runtime graph builder (`Graph::add_node_named`) rejects a duplicate
/// handle anywhere in the WHOLE graph, so a clone whose inner nodes keep
/// their source's exact handles (`mesh_0`, `mat_0`, …) fails to build.
/// Internal wires are re-pointed onto the fresh ids. `exposed_params` is
/// cleared on every cloned node — D11: card exposes are a deliberate act,
/// never carried by a duplicate. `next_id`/`taken` are threaded through so
/// nested clones (a duplicated object's inner mesh/material/transform/
/// scene_object nodes) each get their own fresh id and collision-free
/// handle, ascending. `node_id_map` (BUG-212) collects every (old stable
/// [`NodeId`], new stable `NodeId`) pair produced across the WHOLE subtree —
/// the caller uses it to re-target `string_bindings` entries whose
/// `BindingTarget::Node` falls inside the duplicated subtree onto the
/// clone's fresh ids, so file-dependent nodes (e.g. `node.gltf_mesh_source`)
/// keep their "Model File" path binding on the copy.
fn deep_clone_with_fresh_ids(
    src: &EffectGraphNode,
    next_id: &mut u32,
    taken: &mut std::collections::HashSet<String>,
    node_id_map: &mut Vec<(NodeId, NodeId)>,
) -> EffectGraphNode {
    let mut node = src.clone();
    node.id = *next_id;
    *next_id += 1;
    let old_node_id = node.node_id.clone();
    node.node_id = NodeId::new(manifold_core::short_id());
    node_id_map.push((old_node_id, node.node_id.clone()));
    node.exposed_params = Default::default();
    node.handle = node.handle.as_deref().map(|h| dedup_handle(h, taken));
    if let Some(group) = node.group.as_deref_mut() {
        let mut id_map: Vec<(u32, u32)> = Vec::with_capacity(group.nodes.len());
        let mut new_nodes = Vec::with_capacity(group.nodes.len());
        for n in &group.nodes {
            let old_id = n.id;
            let cloned = deep_clone_with_fresh_ids(n, next_id, taken, node_id_map);
            id_map.push((old_id, cloned.id));
            new_nodes.push(cloned);
        }
        let remap = |id: u32| id_map.iter().find(|(o, _)| *o == id).map(|(_, n)| *n).unwrap_or(id);
        let new_wires: Vec<EffectGraphWire> = group
            .wires
            .iter()
            .map(|w| EffectGraphWire {
                from_node: remap(w.from_node),
                from_port: w.from_port.clone(),
                to_node: remap(w.to_node),
                to_port: w.to_port.clone(),
            })
            .collect();
        group.nodes = new_nodes;
        group.wires = new_wires;
    }
    node
}

/// Resolve the `object_k` wire's producer node id at `wires`' scope — the
/// same "UI's already-resolved index is the one source of truth" lookup
/// [`RemoveSceneObjectCommand`] uses.
fn object_producer_id(wires: &[EffectGraphWire], render_id: u32, k: u32) -> Option<u32> {
    let object_port = format!("object_{k}");
    wires.iter().find(|w| w.to_node == render_id && w.to_port == object_port).map(|w| w.from_node)
}

/// The duplicate-object gesture (D11): one undoable composite edit that
/// deep-clones the source object's `scene_object` (+ its enclosing group,
/// when the object is grouped — the Add/importer shape) with fresh doc ids
/// and fresh [`NodeId`]s throughout, wires the clone's `object` output into
/// the next free `object_k` slot, bumps `objects`, offsets the clone's
/// `node.transform_3d.pos_x` by **+0.5** so it doesn't render exactly inside
/// the original (D11 — deliberate, visible, undoable, tune-by-feel later).
///
/// Ungrouped hand-built objects (a loose `scene_object` whose mesh/
/// transform/material producers are NOT wrapped in a group) share
/// [`RemoveSceneObjectCommand`]'s documented one-hop gap: only the bare
/// `scene_object` node itself is cloned (no upstream producers to walk to —
/// finding them would require a general graph-reachability search this
/// command doesn't attempt), so the clone starts fully unwired. Every
/// object this design's own producers (Add, importer, merge) create is
/// grouped, so this is the shape that actually ships.
#[derive(Debug)]
pub struct DuplicateSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    source_index: u32,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
    /// BUG-212: the WHOLE `preset_metadata.string_bindings` vec before this
    /// edit's append — whole-snapshot undo, same convention as `prev` above.
    /// `None` when the target has no `preset_metadata` at all (nothing to
    /// snapshot, nothing to restore).
    prev_string_bindings: Option<Vec<StringBindingDef>>,
}

impl DuplicateSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        source_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            source_index,
            catalog_default,
            prev: None,
            prev_string_bindings: None,
        }
    }
}

impl Command for DuplicateSceneObjectCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let src_k = self.source_index;
        let mut node_id_map: Vec<(NodeId, NodeId)> = Vec::new();
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let source_id = object_producer_id(wires, render_id, src_k)?;
            let source_node = nodes.iter().find(|n| n.id == source_id)?.clone();

            let mut next_id = max_node_id_over(nodes) + 1;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(nodes, &mut taken);
            let mut clone = deep_clone_with_fresh_ids(&source_node, &mut next_id, &mut taken, &mut node_id_map);
            // D11's exact top-level convention (handle + " 2") overrides
            // whatever `deep_clone_with_fresh_ids`'s generic dedup pass
            // assigned to the TOP node — derived from the SOURCE's own
            // handle, not the post-dedup one (the source's handle is
            // already in `taken`, so a naive dedup on the clone would have
            // produced e.g. "Object 1_2", not the D11 "Object 1 2" shape).
            let cloned_handle = source_node.handle.as_ref().map(|h| format!("{h} 2"));
            clone.handle = cloned_handle.clone();
            clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));

            // D6: the object's name is its scene_object's own handle — when
            // the clone is a group, keep the inner scene_object's handle in
            // sync with the group's (the same invariant Add/importer both
            // maintain, and RenameSceneObjectCommand sweeps to preserve).
            if let Some(body) = clone.group.as_deref_mut() {
                if let Some(inner_object) =
                    body.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")
                {
                    inner_object.handle = cloned_handle;
                }
                // D11: offset the clone's transform_3d.pos_x by +0.5.
                if let Some(transform_node) =
                    body.nodes.iter_mut().find(|n| n.type_id == "node.transform_3d")
                {
                    let cur = match transform_node.params.get("pos_x") {
                        Some(SerializedParamValue::Float { value }) => *value,
                        _ => 0.0,
                    };
                    transform_node
                        .params
                        .insert("pos_x".to_string(), SerializedParamValue::Float { value: cur + 0.5 });
                }
            }

            let current_objects = match nodes.iter().find(|n| n.id == render_id)?.params.get("objects") {
                Some(SerializedParamValue::Float { value }) => *value,
                Some(SerializedParamValue::Int { value }) => *value as f32,
                _ => 0.0,
            };
            let new_k = current_objects as u32;
            let clone_id = clone.id;
            nodes.push(clone);
            wires.push(scene_build_wire(clone_id, "object", render_id, &format!("object_{new_k}")));

            nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                "objects".to_string(),
                SerializedParamValue::Float { value: current_objects + 1.0 },
            );

            Some(prev)
        });
        self.prev = result.flatten();
        if self.prev.is_none() {
            // The clone itself was refused (unresolvable source/level) — no
            // subtree was cloned, so there's nothing to sweep bindings for.
            self.prev_string_bindings = None;
            return;
        }

        // BUG-212: `deep_clone_with_fresh_ids` mints fresh `NodeId`s for
        // every cloned node (D11 — a stale NodeId would let a card binding
        // silently double-drive both the original and the copy), which
        // makes `string_bindings` entries dangle by the same mechanism —
        // unlike `bindings`/`exposed_params` (D11: performer-facing card
        // exposes, deliberately NOT carried by a duplicate), `string_bindings`
        // is the importer's own "Model File" path plumbing (one entry per
        // file-dependent node, fanned out under a shared outer id) and
        // dropping it silently breaks mesh loading on the clone. Clone every
        // entry whose target falls inside the duplicated subtree, re-targeted
        // at the clone's fresh NodeId, same `id`/`label`/`default_value`.
        // Reached at the same undo-unit boundary `RenameSceneObjectCommand`'s
        // D5 sweep uses (`resolve_target_instance`, outside
        // `with_target_graph_mut`'s narrower graph-only view).
        if !node_id_map.is_empty()
            && let Some(inst) = resolve_target_instance(&self.target, project)
            && let Some(meta) = inst.graph.as_mut().and_then(|g| g.preset_metadata.as_mut())
        {
            self.prev_string_bindings = Some(meta.string_bindings.clone());
            let new_entries: Vec<StringBindingDef> = meta
                .string_bindings
                .iter()
                .filter_map(|b| match &b.target {
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => node_id_map
                        .iter()
                        .find(|(old, _)| old == node_id)
                        .map(|(_, new_id)| StringBindingDef {
                            id: b.id.clone(),
                            label: b.label.clone(),
                            default_value: b.default_value.clone(),
                            target: manifold_core::effect_graph_def::BindingTarget::Node {
                                node_id: new_id.clone(),
                                param: param.clone(),
                            },
                        }),
                    manifold_core::effect_graph_def::BindingTarget::Composite { .. } => None,
                })
                .collect();
            meta.string_bindings.extend(new_entries);
        } else {
            self.prev_string_bindings = None;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(prev_sb) = self.prev_string_bindings.clone()
            && let Some(inst) = resolve_target_instance(&self.target, project)
            && let Some(meta) = inst.graph.as_mut().and_then(|g| g.preset_metadata.as_mut())
        {
            meta.string_bindings = prev_sb;
        }

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
        "Duplicate Object"
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
    /// P1/R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new
    /// environment node's full param manifest, computed by the app-side
    /// caller via `manifold_renderer::node_graph::scene_exposure::
    /// metadata_for_node_type("node.bake_environment")` (this crate has no
    /// renderer dep) — same convention `AddSceneLightCommand` uses.
    env_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata`. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl AddSceneEnvironmentCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        pos: (f32, f32),
        env_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            pos,
            env_metadata,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for AddSceneEnvironmentCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let prev_metadata = def.preset_metadata.clone();

            let (env_id, env_node_id, env_node_params, prev) = {
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

                let mut env_node = scene_build_node(
                    env_id,
                    "node.bake_environment",
                    Some("environment".to_string()),
                    params,
                );
                env_node.editor_pos = Some(pos);
                let env_node_id = env_node.node_id.clone();
                let env_node_params = env_node.params.clone();
                nodes.push(env_node);
                wires.push(scene_build_wire(env_id, "envmap", render_id, "envmap"));

                (env_id, env_node_id, env_node_params, prev)
            };

            // R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): expose every
            // param of the freshly minted environment node — same P1 stamp
            // AddSceneLightCommand performs for its own node, into the def's
            // TOP-LEVEL preset_metadata, targeting its bare NodeId. Without
            // this the panel's `world_sections` lookup (`state_sync.rs`'s
            // `sections_for_doc_ids`) comes back empty and
            // `build_filtered_properties` renders nothing for the row.
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
                env_id,
                &env_node_id,
                "node.bake_environment",
                "Environment",
                &self.env_metadata,
                &env_node_params,
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
        let scope = self.scope_path.clone();
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
    /// P1/R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new fog
    /// (atmosphere) node's full param manifest, computed by the app-side
    /// caller via `manifold_renderer::node_graph::scene_exposure::
    /// metadata_for_node_type("node.atmosphere")` (this crate has no
    /// renderer dep) — same convention `AddSceneLightCommand` uses.
    fog_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata`. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl AddSceneFogCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        pos: (f32, f32),
        fog_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            pos,
            fog_metadata,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for AddSceneFogCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let prev_metadata = def.preset_metadata.clone();

            let (fog_id, fog_node_id, prev) = {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                let fog_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                // A freshly-added fog node starts at density 0 (the primitive's own
                // default — "subtle" is authored by hand in the starter preset, not
                // stamped here) so adding it is never a visible surprise; the
                // performer dials density up from the panel immediately after.
                let params = BTreeMap::new();

                let mut fog_node =
                    scene_build_node(fog_id, "node.atmosphere", Some("fog".to_string()), params);
                fog_node.editor_pos = Some(pos);
                let fog_node_id = fog_node.node_id.clone();
                nodes.push(fog_node);
                wires.push(scene_build_wire(fog_id, "atmosphere", render_id, "atmosphere"));

                (fog_id, fog_node_id, prev)
            };

            // R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): expose every
            // param of the freshly minted fog node — same P1 stamp
            // AddSceneLightCommand performs for its own node, into the def's
            // TOP-LEVEL preset_metadata, targeting its bare NodeId. Without
            // this the panel's `world_sections` lookup (`state_sync.rs`'s
            // `sections_for_doc_ids`) comes back empty and
            // `build_filtered_properties` renders nothing for the row —
            // the R1 bug: freshly-added fog was structurally invisible.
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
                fog_id,
                &fog_node_id,
                "node.atmosphere",
                "Atmosphere",
                &self.fog_metadata,
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
        let scope = self.scope_path.clone();
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
        "Add Fog"
    }
}

// ---------------------------------------------------------------------------
// Add Object Transform
// (REALTIME_3D_DESIGN.md P6, D8 amendment "P6 entry state": an object whose
// `transform` port is unwired — SCENE_BUILD_AND_GROUP_PARAMS P2 landed but
// this particular `node.scene_object` was never given a `node.transform_3d`
// — has nothing for the P6 gizmo to write. This command is what the gizmo's
// first axis-grab dispatches before any `SetGraphNodeParamCommand` can
// target the object: spawn a `node.transform_3d` at the scene's graph level
// (identity params — the primitive's own defaults, so creating it alone is
// never a visible surprise, same posture `AddSceneFogCommand` takes) and
// wire its `transform` output into the target `node.scene_object`'s
// `transform` input. Shaped exactly like `AddSceneEnvironmentCommand`
// above; the one difference is the wire target is an object node, not
// `render_scene` itself, and any PRE-EXISTING wire into that `transform`
// port (shouldn't happen — the gizmo only offers this when the Vm traced
// `transform: None` — but defended anyway, same posture
// `override_camera_def` takes for its camera splice) is replaced rather
// than left to dangle into two producers.
// ---------------------------------------------------------------------------

/// "Create transform" (P6): spawn a `node.transform_3d` at the scene's graph
/// level and wire its `transform` output into `scene_object_node_id`'s
/// `transform` input. One undo unit. `created_node_id()` reads back the new
/// node's doc id right after `execute()` so the caller (the gizmo drag
/// handler) can immediately target it with a `SetGraphNodeParamCommand` in
/// the same input event — no round trip through a snapshot needed, since the
/// id assignment (`max existing id + 1`) is exactly what `execute()` used.
#[derive(Debug)]
pub struct AddObjectTransformCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    scene_object_node_id: u32,
    pos: (f32, f32),
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
    created_node_id: Option<u32>,
}

impl AddObjectTransformCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        scene_object_node_id: u32,
        pos: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            scene_object_node_id,
            pos,
            catalog_default,
            prev: None,
            created_node_id: None,
        }
    }

    /// The new `node.transform_3d`'s doc id, valid after `execute()` ran
    /// successfully (i.e. the target/scope resolved). `None` before
    /// `execute()`, or if it failed to resolve (target/scope missing).
    pub fn created_node_id(&self) -> Option<u32> {
        self.created_node_id
    }
}

impl Command for AddObjectTransformCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let object_id = self.scene_object_node_id;
        let pos = self.pos;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            let xf_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
            let params = BTreeMap::new();
            let mut xf_node =
                scene_build_node(xf_id, "node.transform_3d", Some("transform".to_string()), params);
            xf_node.editor_pos = Some(pos);
            nodes.push(xf_node);
            wires.retain(|w| !(w.to_node == object_id && w.to_port == "transform"));
            wires.push(scene_build_wire(xf_id, "transform", object_id, "transform"));

            Some((prev, xf_id))
        });
        match result.flatten() {
            Some((prev, xf_id)) => {
                self.prev = Some(prev);
                self.created_node_id = Some(xf_id);
            }
            None => {
                self.prev = None;
                self.created_node_id = None;
            }
        }
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
        self.created_node_id = None;
    }

    fn description(&self) -> &str {
        "Add Object Transform"
    }
}

// ---------------------------------------------------------------------------
// Import Model into Scene (merge-import)
// (SCENE_SETUP_PANEL_DESIGN.md D5/P4) — "Import Model…" splices a SECOND
// glTF's object groups into an EXISTING scene's `render_scene`, without
// touching that scene's own chrome (camera/envmap/lights/lens). One undo
// unit, shaped exactly like `AddSceneObjectCommand`/`GroupNodesCommand`:
// undo restores the pre-edit `(nodes, wires, preset_metadata)` verbatim.
// ---------------------------------------------------------------------------

/// The plan's data (`new_nodes`/`new_wires`/`new_card_params`/…) is built by
/// `manifold_renderer::node_graph::gltf_import::assemble_merge_plan` /
/// `MergePlan`, which `manifold-editing` cannot depend on (dependency
/// direction — the same constraint `AddSceneObjectCommand`'s own doc
/// comment names for `OBJECT_SAFETY_MAX`). The caller (`manifold-app`,
/// which depends on both crates) builds the plan there and hands its
/// plain `manifold_core` fields to [`ImportModelIntoSceneCommand::new`].
#[derive(Debug)]
pub struct ImportModelIntoSceneCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    new_nodes: Vec<EffectGraphNode>,
    new_wires: Vec<EffectGraphWire>,
    new_objects_count: u32,
    new_card_params: Vec<ParamSpecDef>,
    new_card_bindings: Vec<BindingDef>,
    new_string_bindings: Vec<StringBindingDef>,
    catalog_default: EffectGraphDef,
    /// Pre-edit `(nodes, wires)` at `scope_path`, plus the pre-edit
    /// `preset_metadata` (whole-def field, outside the scoped level) — set
    /// on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl ImportModelIntoSceneCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        new_nodes: Vec<EffectGraphNode>,
        new_wires: Vec<EffectGraphWire>,
        new_objects_count: u32,
        new_card_params: Vec<ParamSpecDef>,
        new_card_bindings: Vec<BindingDef>,
        new_string_bindings: Vec<StringBindingDef>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            new_nodes,
            new_wires,
            new_objects_count,
            new_card_params,
            new_card_bindings,
            new_string_bindings,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for ImportModelIntoSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let new_nodes = self.new_nodes.clone();
        let new_wires = self.new_wires.clone();
        let objects = self.new_objects_count;
        let new_card_params = self.new_card_params.clone();
        let new_card_bindings = self.new_card_bindings.clone();
        let new_string_bindings = self.new_string_bindings.clone();
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let prev_metadata = def.preset_metadata.clone();

            // Card-spec additions land on the WHOLE def's preset_metadata
            // (not the scoped level) — done before descending into scope so
            // the two mutable borrows of `def` (metadata vs. nodes/wires)
            // never overlap.
            if !new_card_params.is_empty()
                || !new_card_bindings.is_empty()
                || !new_string_bindings.is_empty()
            {
                let meta = def.preset_metadata.get_or_insert_with(|| {
                    // Safety net only: every real generator's catalog default
                    // carries a `preset_metadata` (D9) — this arm exists so a
                    // hand-built def with none doesn't silently drop the new
                    // card entries rather than panic.
                    PresetMetadata {
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
                    }
                });
                meta.params.extend(new_card_params);
                meta.bindings.extend(new_card_bindings);
                meta.string_bindings.extend(new_string_bindings);
            }

            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev_nodes_wires = (nodes.clone(), wires.clone());

            nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                "objects".to_string(),
                SerializedParamValue::Float { value: objects as f32 },
            );
            nodes.extend(new_nodes);
            wires.extend(new_wires);

            Some((prev_nodes_wires, prev_metadata))
        });
        if let Some((pnw, pmeta)) = result.flatten() {
            self.prev = Some((pnw.0, pnw.1, pmeta));
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = pmeta;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Import Model into Scene"
    }
}


// ---------------------------------------------------------------------------
// Rename Scene Object / Rename Light (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D6)
// ---------------------------------------------------------------------------

/// The rename-object gesture (D6: "the object IS its `scene_object` node;
/// the name is its `handle`"). One undoable composite edit — extends
/// [`RenameGroupCommand`]'s walk rather than duplicating it: sets the
/// `scene_object` node's own `handle`, ALSO renames the enclosing group when
/// one exists (graph-view coherence — a sweep, not a second home: this
/// command is the single writer of both, same posture D6 states), and runs
/// the same D5 card-section sweep `RenameGroupCommand` runs when a group is
/// renamed. Rejected (a no-op) exactly like `RenameGroupCommand`: an empty
/// name, a name containing `/`, or a collision with a sibling scene_object's
/// or group's handle at the same level.
/// `(scene_object node id, prev scene_object handle, Option<(group node id,
/// prev group handle)>)` — [`RenameSceneObjectCommand`]'s undo snapshot.
type RenameSceneObjectPrev = (u32, Option<String>, Option<(u32, Option<String>)>);

#[derive(Debug)]
pub struct RenameSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    /// The `object_k` wire's producer at `scope_path` — the group when the
    /// object is grouped (Add/importer/merge shape), else the bare
    /// `node.scene_object` itself. Same value `SceneVm`'s
    /// `SceneObjectVm::Known::group_node_id` already resolves to (P1/P2
    /// re-anchored it onto the Object-wire producer, D12), so the panel can
    /// address this command with the exact id it already has — no
    /// render_scene/object-index re-derivation needed. Matches
    /// `RenameGroupCommand::group_node_id`'s addressing shape exactly.
    object_node_id: u32,
    new_handle: String,
    catalog_default: EffectGraphDef,
    /// Captured on first successful execute.
    prev: Option<RenameSceneObjectPrev>,
    /// D5 rename-sweep undo state — same shape as `RenameGroupCommand::swept`.
    /// Only ever populated when the object is grouped (an ungrouped bare
    /// scene_object has no group name for a card section to have followed).
    swept: Vec<(String, Option<String>)>,
}

impl RenameSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        object_node_id: u32,
        new_handle: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self { target, scope_path, object_node_id, new_handle, catalog_default, prev: None, swept: Vec::new() }
    }
}

impl Command for RenameSceneObjectCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let producer_id = self.object_node_id;
        let new_handle = self.new_handle.clone();
        let first_time = self.prev.is_none();

        let captured = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            if new_handle.is_empty() || new_handle.contains('/') {
                return None;
            }
            // Reject a collision with any sibling's handle at this level
            // (matching RenameGroupCommand's own guard).
            if nodes
                .iter()
                .any(|n| n.id != producer_id && n.handle.as_deref() == Some(new_handle.as_str()))
            {
                return None;
            }
            let producer = nodes.iter_mut().find(|n| n.id == producer_id)?;

            if producer.type_id == GROUP_TYPE_ID {
                // Grouped shape (Add / importer / merge): rename the group
                // AND the inner scene_object's own handle stays in sync
                // (D6's single-writer-of-both posture).
                let prev_group_handle = producer.handle.clone();
                producer.handle = Some(new_handle.clone());
                let body = producer.group.as_deref_mut()?;
                let scene_object = body.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")?;
                let scene_object_id = scene_object.id;
                let prev_object_handle = scene_object.handle.clone();
                scene_object.handle = Some(new_handle.clone());

                let mut inside = Vec::new();
                collect_node_ids(&body.nodes, &mut inside);
                Some((scene_object_id, prev_object_handle, Some((producer_id, prev_group_handle)), inside))
            } else {
                // Ungrouped bare scene_object: just its own handle, no group
                // to keep in sync, no card-section sweep possible.
                let prev_object_handle = producer.handle.clone();
                producer.handle = Some(new_handle.clone());
                Some((producer_id, prev_object_handle, None, Vec::new()))
            }
        });
        let Some((scene_object_id, prev_object_handle, prev_group, inside)) = captured.flatten() else {
            return;
        };
        if first_time {
            self.prev = Some((scene_object_id, prev_object_handle, prev_group.clone()));
        }
        if !first_time {
            return;
        }

        // D5 sweep — only runs when the object is grouped (`prev_group` is
        // `Some`) and had a prior name (nothing could be sectioned under an
        // unnamed group).
        let Some(old_name) = prev_group.and_then(|(_, prev_handle)| prev_handle) else {
            return;
        };
        let Some(inst) = resolve_target_instance(&self.target, project) else {
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
                p.spec.section = Some(self.new_handle.clone());
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.swept.is_empty()
            && let Some(inst) = resolve_target_instance(&self.target, project)
        {
            for (param_id, prev_section) in self.swept.drain(..) {
                if let Some(p) = inst.params.get_mut(&param_id) {
                    p.spec.section = prev_section;
                }
            }
        }

        let Some((scene_object_id, prev_object_handle, prev_group)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) else {
                return;
            };
            if let Some((group_id, prev_group_handle)) = prev_group {
                if let Some(group) = nodes.iter_mut().find(|n| n.id == group_id) {
                    group.handle = prev_group_handle;
                    if let Some(body) = group.group.as_deref_mut()
                        && let Some(scene_object) =
                            body.nodes.iter_mut().find(|n| n.id == scene_object_id)
                    {
                        scene_object.handle = prev_object_handle;
                    }
                }
            } else if let Some(node) = nodes.iter_mut().find(|n| n.id == scene_object_id) {
                node.handle = prev_object_handle;
            }
        });
    }

    fn description(&self) -> &str {
        "Rename Object"
    }
}

/// Plain rename of a node's `handle` — no card-section sweep (D6: nothing
/// downstream displays light names today, unlike an object's group). Used
/// for `node.light`'s name; a generic, single-purpose sibling of the
/// heavier `RenameSceneObjectCommand`.
#[derive(Debug)]
pub struct SetNodeHandleCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    node_doc_id: u32,
    new_handle: String,
    catalog_default: EffectGraphDef,
    prev: Option<Option<String>>,
}

impl SetNodeHandleCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        node_doc_id: u32,
        new_handle: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self { target, scope_path, node_doc_id, new_handle, catalog_default, prev: None }
    }
}

impl Command for SetNodeHandleCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.node_doc_id;
        let new_handle = self.new_handle.clone();
        let first_time = self.prev.is_none();
        let captured = with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
            let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            if new_handle.is_empty() || new_handle.contains('/') {
                return None;
            }
            if nodes.iter().any(|n| n.id != id && n.handle.as_deref() == Some(new_handle.as_str())) {
                return None;
            }
            let node = nodes.iter_mut().find(|n| n.id == id)?;
            let prev = node.handle.clone();
            node.handle = Some(new_handle.clone());
            Some(prev)
        });
        if first_time {
            self.prev = captured.flatten();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let id = self.node_doc_id;
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == id)
            {
                node.handle = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Rename Light"
    }
}


#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::test_support::*;
    use manifold_core::LayerId;
    use manifold_core::PresetTypeId;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
    use manifold_core::effect_graph_def::{BindingDef, GROUP_TYPE_ID, ParamSpecDef, PresetMetadata, SkipModeDef, StringBindingDef};
    use crate::command::Command;

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
            card_visible: true,
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
}
