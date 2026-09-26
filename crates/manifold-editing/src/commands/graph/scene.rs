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
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, ParamSpecDef, PresetMetadata, SerializedParamValue, StringBindingDef,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{SceneParamMetadata, stamp_scene_node_exposures_into};

use crate::command::Command;

mod clone_sections;

use super::{
    InstanceLayerSnapshot, collect_node_ids, dedup_handle, descend_level, prune_instance_params,
    refresh_target_manifest, resolve_target_instance, scene_build_node, scene_build_wire,
    with_existing_target_graph_mut, with_target_graph_def_mut, with_target_graph_mut,
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
    /// When present, Add Object also creates a loose physics object in the
    /// one Physics World in the current scope. The renderer metadata is kept
    /// caller supplied because editing has no renderer dependency.
    physics_body_metadata: Option<Vec<SceneParamMetadata>>,
    physics_material_metadata: Option<Vec<SceneParamMetadata>>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata` (P1 exposure stamping lands there, outside
    /// the scoped level). Set on execute.
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    rejection: Option<&'static str>,
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
            physics_body_metadata: None,
            physics_material_metadata: None,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }

    /// Request the physics-aware Add Object shape. If this is used in a
    /// scope with no Physics World, the existing visual-only shape is kept.
    /// A scope with more than one world is rejected rather than guessing.
    pub fn with_physics_world(
        mut self,
        rigid_body_metadata: Vec<SceneParamMetadata>,
        pbr_material_metadata: Vec<SceneParamMetadata>,
    ) -> Self {
        self.physics_body_metadata = Some(rigid_body_metadata);
        self.physics_material_metadata = Some(pbr_material_metadata);
        self
    }

    fn physics_world_for_scope(
        &self,
        project: &Project,
    ) -> Result<Option<(u32, u32)>, &'static str> {
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return Ok(None);
        };
        let Some((nodes, wires)) = graph_level(def, &self.scope_path) else {
            return Ok(None);
        };
        let worlds: Vec<u32> = nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .map(|node| node.id)
            .collect();
        let Some(world_id) = worlds.first().copied() else {
            return Ok(None);
        };
        if !self.scope_path.is_empty() {
            return Err("Physics objects currently require a root-level scene");
        }
        if worlds.len() != 1 {
            return Err("Add Object requires exactly one Physics World in the current scope");
        }
        let Some(body_slot) = first_free_physics_body_slot(wires, world_id) else {
            return Err("Physics World has no free body slots");
        };
        Ok(Some((world_id, body_slot)))
    }

    fn execute_physics(&mut self, project: &mut Project, world_id: u32, body_slot: u32) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let centroid = self.centroid;
        let Some(body_metadata) = self.physics_body_metadata.as_ref() else {
            return;
        };
        let Some(material_metadata) = self.physics_material_metadata.as_ref() else {
            return;
        };
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let last_id = max_node_id_over(&def.nodes).checked_add(5)?;
                let mut taken = std::collections::HashSet::new();
                collect_all_handles(&def.nodes, &mut taken);
                let prev_metadata = def.preset_metadata.clone();
                let (added, prev) = {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let prev = (nodes.clone(), wires.clone());
                    if !nodes.iter().any(|node| node.id == render_id) {
                        return None;
                    }
                    let added = append_physics_scene_object(
                        nodes, wires, render_id, k, world_id, body_slot, last_id, centroid,
                        &mut taken,
                    );
                    nodes
                        .iter_mut()
                        .find(|node| node.id == render_id)?
                        .params
                        .insert(
                            "objects".to_string(),
                            SerializedParamValue::Float {
                                value: (k + 1) as f32,
                            },
                        );
                    (added, prev)
                };
                let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
                    id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
                    display_name: "Scene".to_string(),
                    category: "Geometry".to_string(),
                    osc_prefix: "scene".to_string(),
                    legacy_discriminant: None,
                    available: true,
                    is_line_based: false,
                    layer_types: None,
                    params: Vec::new(),
                    bindings: Vec::new(),
                    param_aliases: Vec::new(),
                    value_aliases: Vec::new(),
                    string_params: Vec::new(),
                    string_bindings: Vec::new(),
                    scene_modifier: None,
                    scene_bounds: None,
                });
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    added.material_id,
                    &added.material_node_id,
                    "node.pbr_material",
                    &format!("{} — Material", added.handle),
                    material_metadata,
                    &added.material_params,
                );
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    added.transform_id,
                    &added.transform_node_id,
                    "node.transform_3d",
                    &format!("{} — Transform", added.handle),
                    &self.transform_metadata,
                    &added.transform_params,
                );
                if let Some((body_id, body_node_id, body_params)) = added.physics_body {
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        body_id,
                        &body_node_id,
                        "node.rigid_body",
                        &format!("{} — Rigid Body", added.handle),
                        body_metadata,
                        &body_params,
                    );
                }
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    added.scene_object_id,
                    &added.scene_object_node_id,
                    "node.scene_object",
                    &added.handle,
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
}

struct AddedSceneObject {
    material_id: u32,
    material_node_id: NodeId,
    material_params: BTreeMap<String, SerializedParamValue>,
    transform_id: u32,
    transform_node_id: NodeId,
    transform_params: BTreeMap<String, SerializedParamValue>,
    scene_object_id: u32,
    scene_object_node_id: NodeId,
    handle: String,
    physics_body: Option<(u32, NodeId, BTreeMap<String, SerializedParamValue>)>,
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

fn append_physics_scene_object(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    render_id: u32,
    object_index: u32,
    world_id: u32,
    body_slot: u32,
    last_id: u32,
    centroid: (f32, f32),
    taken: &mut std::collections::HashSet<String>,
) -> AddedSceneObject {
    let handle = dedup_handle(&format!("Object {}", object_index + 1), taken);
    let transform_handle = dedup_handle(&format!("{handle} Transform"), taken);
    let body_handle = dedup_handle(&format!("{handle} Body"), taken);
    let mesh_handle = dedup_handle(&format!("{handle} Mesh"), taken);
    let material_handle = dedup_handle(&format!("{handle} Material"), taken);

    let transform_id = last_id - 4;
    let body_id = last_id - 3;
    let mesh_id = last_id - 2;
    let material_id = last_id - 1;
    let scene_object_id = last_id;
    let tint = scene_object_tint(object_index);
    let mut body_params = BTreeMap::new();
    body_params.insert("shape".to_string(), SerializedParamValue::Enum { value: 1 });
    body_params.insert(
        "motion".to_string(),
        SerializedParamValue::Enum { value: 1 },
    );
    body_params.insert(
        "mass".to_string(),
        SerializedParamValue::Float { value: 1.0 },
    );
    body_params.insert(
        "friction".to_string(),
        SerializedParamValue::Float { value: 0.5 },
    );
    body_params.insert(
        "bounce".to_string(),
        SerializedParamValue::Float { value: 0.15 },
    );
    let mut material_params = BTreeMap::new();
    material_params.insert(
        "color_r".to_string(),
        SerializedParamValue::Float { value: tint.r },
    );
    material_params.insert(
        "color_g".to_string(),
        SerializedParamValue::Float { value: tint.g },
    );
    material_params.insert(
        "color_b".to_string(),
        SerializedParamValue::Float { value: tint.b },
    );
    let mut transform_params = BTreeMap::new();
    transform_params.insert(
        "pos_y".to_string(),
        SerializedParamValue::Float { value: 2.0 },
    );

    let mut transform = scene_build_node(
        transform_id,
        "node.transform_3d",
        Some(transform_handle),
        transform_params.clone(),
    );
    transform.editor_pos = Some(centroid);
    let body = scene_build_node(
        body_id,
        "node.rigid_body",
        Some(body_handle),
        body_params.clone(),
    );
    let mesh = scene_build_node(
        mesh_id,
        "node.platonic_solid_mesh",
        Some(mesh_handle),
        BTreeMap::new(),
    );
    let material = scene_build_node(
        material_id,
        "node.pbr_material",
        Some(material_handle),
        material_params.clone(),
    );
    let object = scene_build_node(
        scene_object_id,
        "node.scene_object",
        Some(handle.clone()),
        BTreeMap::new(),
    );
    let transform_node_id = transform.node_id.clone();
    let body_node_id = body.node_id.clone();
    let material_node_id = material.node_id.clone();
    let scene_object_node_id = object.node_id.clone();
    nodes.extend([transform, body, mesh, material, object]);
    let wire = |from_node, from_port: &str, to_node, to_port: String| EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port,
    };
    wires.extend([
        wire(transform_id, "transform", body_id, "transform".to_string()),
        wire(body_id, "body", world_id, format!("body_{body_slot}")),
        wire(body_id, "shape", mesh_id, "shape".to_string()),
        wire(mesh_id, "vertices", scene_object_id, "vertices".to_string()),
        wire(material_id, "out", scene_object_id, "material".to_string()),
        wire(
            world_id,
            &format!("pose_{body_slot}"),
            scene_object_id,
            "transform".to_string(),
        ),
        wire(
            scene_object_id,
            "object",
            render_id,
            format!("object_{object_index}"),
        ),
    ]);
    AddedSceneObject {
        material_id,
        material_node_id,
        material_params,
        transform_id,
        transform_node_id,
        transform_params,
        scene_object_id,
        scene_object_node_id,
        handle,
        physics_body: Some((body_id, body_node_id, body_params)),
    }
}

impl Command for AddSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        if self.physics_body_metadata.is_some() {
            match self.physics_world_for_scope(project) {
                Ok(Some((world_id, body_slot))) => {
                    self.execute_physics(project, world_id, body_slot);
                    return;
                }
                Err(reason) => {
                    self.rejection = Some(reason);
                    return;
                }
                Ok(None) => {}
            }
        }
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let centroid = self.centroid;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let prev_metadata = def.preset_metadata.clone();

                // Document ids are global even when the edit targets a nested
                // level. Allocate the complete six-id block before borrowing
                // that level so a full document or id exhaustion leaves the
                // graph untouched.
                let mut next_id = max_node_id_over(&def.nodes).checked_add(1);
                let mut fresh = || -> Option<u32> {
                    let id = next_id?;
                    next_id = id.checked_add(1);
                    Some(id)
                };
                let mesh_id = fresh()?;
                let mat_id = fresh()?;
                let transform_id = fresh()?;
                let scene_object_id = fresh()?;
                let out_id = fresh()?;
                let group_id = fresh()?;

                // Build the group + wire it in, entirely within a nested block so
                // the `nodes`/`wires` borrows (from `descend_level`) end before
                // the P1 exposure stamping below touches `def.preset_metadata` —
                // same "metadata vs. nodes/wires never overlap" discipline
                // `ImportModelIntoSceneCommand` documents.
                let (
                    mat_id,
                    mat_node_id,
                    mat_node_params,
                    transform_id,
                    transform_node_id,
                    scene_object_id,
                    scene_object_node_id,
                    handle,
                    prev,
                ) = {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let prev = (nodes.clone(), wires.clone());

                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: (k + 1) as f32,
                        },
                    );

                    let tint = scene_object_tint(k);
                    let mut mat_params = BTreeMap::new();
                    mat_params.insert(
                        "color_r".to_string(),
                        SerializedParamValue::Float { value: tint.r },
                    );
                    mat_params.insert(
                        "color_g".to_string(),
                        SerializedParamValue::Float { value: tint.g },
                    );
                    mat_params.insert(
                        "color_b".to_string(),
                        SerializedParamValue::Float { value: tint.b },
                    );

                    let mesh_node = scene_build_node(
                        mesh_id,
                        "node.cube_mesh",
                        Some(format!("mesh_{k}")),
                        BTreeMap::new(),
                    );
                    let mat_node = scene_build_node(
                        mat_id,
                        "node.phong_material",
                        Some(format!("mat_{k}")),
                        mat_params,
                    );
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
                    let scene_object_node = scene_build_node(
                        scene_object_id,
                        "node.scene_object",
                        Some(handle.clone()),
                        BTreeMap::new(),
                    );
                    let scene_object_node_id = scene_object_node.node_id.clone();
                    let out_node =
                        scene_build_node(out_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());

                    let group_wires = vec![
                        scene_build_wire(mesh_id, "vertices", scene_object_id, "vertices"),
                        scene_build_wire(mat_id, "out", scene_object_id, "material"),
                        scene_build_wire(transform_id, "transform", scene_object_id, "transform"),
                        scene_build_wire(scene_object_id, "object", out_id, "object"),
                    ];

                    let mut group_node = scene_build_node(
                        group_id,
                        GROUP_TYPE_ID,
                        Some(handle.clone()),
                        BTreeMap::new(),
                    );
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
                        nodes: vec![
                            mesh_node,
                            mat_node,
                            transform_node,
                            scene_object_node,
                            out_node,
                        ],
                        wires: group_wires,
                        tint: Some([tint.r, tint.g, tint.b, 1.0]),
                    }));

                    nodes.push(group_node);
                    wires.push(scene_build_wire(
                        group_id,
                        "object",
                        render_id,
                        &format!("object_{k}"),
                    ));

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
                    layer_types: None,
                    params: Vec::new(),
                    bindings: Vec::new(),
                    param_aliases: Vec::new(),
                    value_aliases: Vec::new(),
                    string_params: Vec::new(),
                    string_bindings: Vec::new(),
                    scene_modifier: None,
                    scene_bounds: None,
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

    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
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
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let pos = self.pos;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let prev_metadata = def.preset_metadata.clone();

                let (light_id, light_node_id, light_node_params, prev) = {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let prev = (nodes.clone(), wires.clone());

                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
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
                    params.insert(
                        "pos_x".to_string(),
                        SerializedParamValue::Float { value: 0.0 },
                    );
                    params.insert(
                        "pos_y".to_string(),
                        SerializedParamValue::Float { value: 7.0 },
                    );
                    params.insert(
                        "pos_z".to_string(),
                        SerializedParamValue::Float { value: 7.0 },
                    );
                    params.insert(
                        "color_r".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );
                    params.insert(
                        "color_g".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );
                    params.insert(
                        "color_b".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );
                    params.insert(
                        "intensity".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );
                    params.insert(
                        "cast_shadows".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );

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
                    wires.push(scene_build_wire(
                        light_id,
                        "out",
                        render_id,
                        &format!("light_{k}"),
                    ));

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
                    layer_types: None,
                    params: Vec::new(),
                    bindings: Vec::new(),
                    param_aliases: Vec::new(),
                    value_aliases: Vec::new(),
                    string_params: Vec::new(),
                    string_bindings: Vec::new(),
                    scene_modifier: None,
                    scene_bounds: None,
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
fn shift_indexed_ports_down(
    wires: &mut [EffectGraphWire],
    to_node: u32,
    prefix: &str,
    removed_index: u32,
) {
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
    rejection: Option<&'static str>,
    /// The level and metadata before this edit, plus the host instance state
    /// that is pruned when the removed object's exposures disappear.
    prev: Option<RemovedObjectSnapshot>,
}

#[derive(Debug, Clone)]
struct RemovedObjectSnapshot {
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    metadata: Option<PresetMetadata>,
    instance: InstanceLayerSnapshot,
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
            rejection: None,
            prev: None,
        }
    }
}

impl Command for RemoveSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return;
        };
        if deletion_breaks_explicit_modifier_target(
            def,
            &self.scope_path,
            self.render_scene_node_id,
            self.object_index,
        ) {
            self.rejection = Some(
                "Object is explicitly targeted by a scene modifier; retarget or remove that modifier first",
            );
            return;
        }
        let Some(previous_instance) = resolve_target_instance(&self.target, project)
            .map(|instance| InstanceLayerSnapshot::capture(instance))
        else {
            return;
        };
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.object_index;
        let physics_match = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .and_then(|def| {
                let (nodes, wires) = graph_level(def, &scope)?;
                let source_id = object_producer_id(wires, render_id, k)?;
                Some(physics_scene_object_match(
                    nodes, wires, render_id, k, source_id,
                ))
            });
        if let Some(PhysicsSceneObjectMatch::Malformed(reason)) = &physics_match {
            self.rejection = Some(reason);
            return;
        }
        if matches!(
            physics_match.as_ref(),
            Some(PhysicsSceneObjectMatch::Valid(_))
        ) && !scope.is_empty()
        {
            self.rejection = Some("Remove Object physics ownership requires a root-level scene");
            return;
        }
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let prev_metadata = def.preset_metadata.clone();
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                let object_port = format!("object_{k}");
                let producer_id = wires
                    .iter()
                    .find(|w| w.to_node == render_id && w.to_port == object_port)
                    .map(|w| w.from_node)?;

                let current_objects = match nodes
                    .iter()
                    .find(|n| n.id == render_id)?
                    .params
                    .get("objects")
                {
                    Some(SerializedParamValue::Float { value }) => *value,
                    Some(SerializedParamValue::Int { value }) => *value as f32,
                    _ => return None,
                };

                let producer = nodes.iter().find(|node| node.id == producer_id)?;
                let removed_indices = match physics_match.as_ref() {
                    Some(PhysicsSceneObjectMatch::Valid(physics)) => physics.render_indices.clone(),
                    _ if producer.type_id == GROUP_TYPE_ID => {
                        group_render_indices(wires, render_id, producer_id)
                    }
                    _ => vec![k],
                };
                if removed_indices.is_empty() {
                    return None;
                }
                let mut removed_ids = Vec::new();
                if let Some(PhysicsSceneObjectMatch::Valid(physics)) = &physics_match {
                    for id in &physics.owned_ids {
                        if let Some(node) = nodes.iter().find(|node| node.id == *id) {
                            collect_node_ids(std::slice::from_ref(node), &mut removed_ids);
                        }
                    }
                    let owned = physics
                        .owned_ids
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>();
                    nodes.retain(|node| !owned.contains(&node.id));
                    wires.retain(|wire| {
                        !owned.contains(&wire.from_node) && !owned.contains(&wire.to_node)
                    });
                } else {
                    collect_node_ids(std::slice::from_ref(producer), &mut removed_ids);
                    nodes.retain(|n| n.id != producer_id);
                    wires.retain(|w| {
                        w.from_node != producer_id
                            && w.to_node != producer_id
                            && !(w.to_node == render_id
                                && removed_indices.iter().any(|index| {
                                    w.to_port == format!("object_{index}")
                                }))
                    });
                }

                // Compact in reverse order so each removal is applied to the
                // original slot numbering without shifting a later target
                // before it is removed.
                for index in removed_indices.iter().rev() {
                    shift_indexed_ports_down(wires, render_id, "object", *index);
                }

                nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: (current_objects - removed_indices.len() as f32).max(0.0),
                    },
                );

                let removed_params = prune_scene_object_metadata(def, &removed_ids);
                Some((prev, prev_metadata, removed_params))
            });
        let Some((prev, prev_metadata, removed_param_ids)) = result.flatten() else {
            return;
        };
        if let Some(instance) = resolve_target_instance(&self.target, project) {
            prune_instance_params(instance, &removed_param_ids);
        }
        self.prev = Some(RemovedObjectSnapshot {
            nodes: prev.0,
            wires: prev.1,
            metadata: prev_metadata,
            instance: previous_instance,
        });
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(snapshot) = self.prev.take() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = snapshot.metadata;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = snapshot.nodes;
                *wires = snapshot.wires;
            }
        });
        if let Some(instance) = resolve_target_instance(&self.target, project) {
            snapshot.instance.restore(instance);
        }
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Remove Object"
    }

    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }
}

#[derive(Debug, Clone)]
struct CompoundChildInfo {
    group_id: u32,
    output_id: u32,
    output_port: String,
    child_id: u32,
    output_ports: Vec<String>,
}

fn compound_child_info(
    def: &EffectGraphDef,
    render_id: u32,
    render_index: u32,
) -> Result<CompoundChildInfo, &'static str> {
    let render_wire = def
        .wires
        .iter()
        .find(|wire| wire.to_node == render_id && wire.to_port == format!("object_{render_index}"))
        .ok_or("Selected submesh render output is unavailable")?;
    let group_id = render_wire.from_node;
    let group_node = def
        .nodes
        .iter()
        .find(|node| node.id == group_id && node.type_id == GROUP_TYPE_ID)
        .ok_or("Selected submesh is not inside an editable object group")?;
    let group = group_node
        .group
        .as_deref()
        .ok_or("Selected submesh group is malformed")?;
    let output_port = render_wire
        .from_port
        .strip_prefix("object")
        .map(|suffix| if suffix.is_empty() { "object".to_string() } else { format!("object{suffix}") })
        .ok_or("Selected submesh group output is malformed")?;
    let output_id = group
        .wires
        .iter()
        .find(|wire| {
            wire.to_port == output_port
                && group
                    .nodes
                    .iter()
                    .any(|node| node.id == wire.to_node && node.type_id == GROUP_OUTPUT_TYPE_ID)
        })
        .map(|wire| wire.to_node)
        .ok_or("Selected submesh group has no output boundary")?;
    let child_id = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output_id && wire.to_port == output_port)
        .map(|wire| wire.from_node)
        .ok_or("Selected submesh group output is unwired")?;
    if !group
        .nodes
        .iter()
        .any(|node| node.id == child_id && node.type_id == "node.scene_object")
    {
        return Err("Selected submesh output is not a scene object");
    }
    let output_ports = group
        .interface
        .outputs
        .iter()
        .filter(|port| port.port_type == "Object")
        .map(|port| port.name.clone())
        .collect::<Vec<_>>();
    if !output_ports.iter().any(|port| port == &output_port) {
        return Err("Selected submesh group interface is malformed");
    }
    Ok(CompoundChildInfo {
        group_id,
        output_id,
        output_port,
        child_id,
        output_ports,
    })
}

fn upstream_ids_for_child(group: &GroupDef, child_id: u32) -> std::collections::HashSet<u32> {
    let mut ids = std::collections::HashSet::from([child_id]);
    let mut stack = vec![child_id];
    while let Some(to_node) = stack.pop() {
        for wire in group.wires.iter().filter(|wire| wire.to_node == to_node) {
            if wire.to_port == "parent_transform" || wire.to_port == "parent_visible" {
                continue;
            }
            let Some(source) = group.nodes.iter().find(|node| node.id == wire.from_node) else {
                continue;
            };
            if source.type_id == GROUP_INPUT_TYPE_ID || source.type_id == GROUP_OUTPUT_TYPE_ID {
                continue;
            }
            if ids.insert(source.id) {
                stack.push(source.id);
            }
        }
    }
    ids
}

fn child_owned_ids(group: &GroupDef, child_id: u32) -> std::collections::HashSet<u32> {
    let sibling_ids = group
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_object" && node.id != child_id)
        .flat_map(|node| upstream_ids_for_child(group, node.id))
        .collect::<std::collections::HashSet<_>>();
    upstream_ids_for_child(group, child_id)
        .into_iter()
        .filter(|id| *id == child_id || !sibling_ids.contains(id))
        .collect()
}

fn output_port_index(port: &str) -> Option<usize> {
    if port == "object" {
        Some(0)
    } else {
        port.strip_prefix("object_")?.parse().ok()
    }
}

fn object_output_port(index: usize) -> String {
    if index == 0 {
        "object".to_string()
    } else {
        format!("object_{index}")
    }
}

fn shift_group_output_port(port: &mut String, removed: usize) {
    let Some(index) = output_port_index(port) else {
        return;
    };
    if index > removed {
        *port = object_output_port(index - 1);
    }
}

fn sync_group_physics_compound(group: &mut GroupDef) -> Result<(), &'static str> {
    let body_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.rigid_body")
        .map(|node| node.id);
    let Some(body_id) = body_id else {
        return Ok(());
    };
    let mut sources = Vec::new();
    let mut transforms = Vec::new();
    for port in group.interface.outputs.iter().filter(|port| port.port_type == "Object") {
        let child_id = object_node_for_group_output(group, &port.name)
            .ok_or("Physics compound child output is unavailable")?;
        sources.push(imported_source_in_level(&group.nodes, &group.wires, child_id)?);
        let transform_wire = group
            .wires
            .iter()
            .find(|wire| wire.to_node == child_id && wire.to_port == "transform")
            .ok_or("Physics compound child transform is unavailable")?;
        let transform = group
            .nodes
            .iter()
            .find(|node| node.id == transform_wire.from_node && node.type_id == "node.transform_3d")
            .ok_or("Physics compound child transform is malformed")?;
        transforms.push(transform.id);
    }
    let materials = compound_materials_param(&sources)?;
    group.wires.retain(|wire| !(wire.to_node == body_id && wire.to_port.starts_with("part_")));
    for (index, transform_id) in transforms.into_iter().enumerate() {
        group.wires.push(scene_build_wire(
            transform_id,
            "transform",
            body_id,
            &format!("part_{index}"),
        ));
    }
    if let Some(body) = group.nodes.iter_mut().find(|node| node.id == body_id) {
        body.params.insert("compound_materials".to_string(), materials);
    }
    Ok(())
}

fn preserve_shared_parent_visible_binding(
    def: &mut EffectGraphDef,
    source_node_id: &NodeId,
    cloned_node_id: &NodeId,
) {
    let Some(meta) = def.preset_metadata.as_mut() else {
        return;
    };
    let shared = meta
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param }
                if node_id == source_node_id && param == "parent_visible" =>
            {
                Some(binding.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if shared.is_empty() {
        return;
    }
    let mut removed_ids = std::collections::HashSet::new();
    meta.bindings.retain(|binding| {
        if matches!(
            &binding.target,
            BindingTarget::Node { node_id, param }
                if node_id == cloned_node_id && param == "parent_visible"
        ) {
            removed_ids.insert(binding.id.clone());
            false
        } else {
            true
        }
    });
    meta.params.retain(|param| !removed_ids.contains(&param.id));
    for mut binding in shared {
        binding.target = BindingTarget::Node {
            node_id: cloned_node_id.clone(),
            param: "parent_visible".to_string(),
        };
        meta.bindings.push(binding);
    }
}

/// Remove exactly one material output from a compound group.  The group is
/// retained while siblings remain, so shared parent transform/visibility
/// inputs and sibling mesh/material chains survive untouched.
#[derive(Debug)]
pub struct RemoveSceneSubmeshCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    physical_index: u32,
    catalog_default: EffectGraphDef,
    prev: Option<RemovedObjectSnapshot>,
    rejection: Option<String>,
}

impl RemoveSceneSubmeshCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        physical_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self { target, render_scene_node_id, physical_index, catalog_default, prev: None, rejection: None }
    }
}

impl Command for RemoveSceneSubmeshCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) { targets.push(self.target.clone()); }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else { return; };
        if deletion_breaks_explicit_modifier_target(def, &[], self.render_scene_node_id, self.physical_index) {
            self.rejection = Some("Submesh is explicitly targeted by a scene modifier; retarget or remove that modifier first".into());
            return;
        }
        let info = match compound_child_info(def, self.render_scene_node_id, self.physical_index) {
            Ok(info) => info,
            Err(reason) => { self.rejection = Some(reason.into()); return; }
        };
        let Some(instance) = resolve_target_instance(&self.target, project).map(|instance| InstanceLayerSnapshot::capture(instance)) else { return; };
        let render_id = self.render_scene_node_id;
        let physical_index = self.physical_index;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let previous_metadata = def.preset_metadata.clone();
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &[])?;
            let previous = (nodes.clone(), wires.clone());
            let group_index = nodes.iter().position(|node| node.id == info.group_id)?;
            let group = nodes[group_index].group.as_deref()?.clone();
            let owned = child_owned_ids(&group, info.child_id);
            let remove_group = info.output_ports.len() <= 1;
            let mut removed_node_ids = Vec::new();
            if remove_group {
                collect_node_ids(std::slice::from_ref(&nodes[group_index]), &mut removed_node_ids);
                nodes.retain(|node| node.id != info.group_id);
                wires.retain(|wire| wire.from_node != info.group_id && wire.to_node != info.group_id);
            } else {
                let removed_ids: std::collections::HashSet<_> = owned.iter().copied().collect();
                for node in &group.nodes {
                    if removed_ids.contains(&node.id) { collect_node_ids(std::slice::from_ref(node), &mut removed_node_ids); }
                }
                let mut group = group;
                group.nodes.retain(|node| !removed_ids.contains(&node.id));
                group.wires.retain(|wire| {
                    !removed_ids.contains(&wire.from_node)
                        && !removed_ids.contains(&wire.to_node)
                        && (wire.to_node != info.output_id || wire.to_port != info.output_port)
                });
                let removed_output = output_port_index(&info.output_port)?;
                group.interface.outputs.retain(|port| port.name != info.output_port);
                for port in &mut group.interface.outputs { shift_group_output_port(&mut port.name, removed_output); }
                for wire in &mut group.wires { shift_group_output_port(&mut wire.to_port, removed_output); }
                sync_group_physics_compound(&mut group).ok()?;
                for wire in wires.iter_mut().filter(|wire| wire.from_node == info.group_id) {
                    shift_group_output_port(&mut wire.from_port, removed_output);
                }
                nodes[group_index].group = Some(Box::new(group));
            }
            wires.retain(|wire| !(wire.to_node == render_id && wire.to_port == format!("object_{physical_index}")));
            for wire in wires.iter_mut().filter(|wire| wire.to_node == render_id) {
                shift_indexed_port(wire, "object", physical_index);
            }
            let render = nodes.iter_mut().find(|node| node.id == render_id)?;
            let count = match render.params.get("objects") {
                Some(SerializedParamValue::Float { value }) => *value,
                Some(SerializedParamValue::Int { value }) => *value as f32,
                _ => return None,
            };
            render.params.insert("objects".into(), SerializedParamValue::Float { value: (count - 1.0).max(0.0) });
            let removed_params = prune_scene_object_metadata(def, &removed_node_ids);
            Some((previous, previous_metadata, removed_params))
        });
        let Some((previous, metadata, removed_params)) = result.flatten() else { self.rejection = Some("Submesh graph changed before removal".into()); return; };
        if let Some(instance) = resolve_target_instance(&self.target, project) { prune_instance_params(instance, &removed_params); }
        self.prev = Some(RemovedObjectSnapshot { nodes: previous.0, wires: previous.1, metadata, instance });
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(snapshot) = self.prev.take() else { return; };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = snapshot.metadata;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &[]) { *nodes = snapshot.nodes; *wires = snapshot.wires; }
        });
        if let Some(instance) = resolve_target_instance(&self.target, project) { snapshot.instance.restore(instance); }
        refresh_target_manifest(project, &self.target);
    }
    fn description(&self) -> &str { "Remove Submesh" }
    fn was_applied(&self) -> bool { self.prev.is_some() }
    fn rejection_reason(&self) -> Option<&str> { self.rejection.as_deref() }
}

fn shift_indexed_port(wire: &mut EffectGraphWire, prefix: &str, removed: u32) {
    let needle = format!("{prefix}_");
    if let Some(index) = wire.to_port.strip_prefix(&needle).and_then(|value| value.parse::<u32>().ok())
        && index > removed
    {
        wire.to_port = format!("{prefix}_{}", index - 1);
    }
}

/// Duplicate one compound child into the same parent group and append its
/// Object boundary at the next physical render slot.
#[derive(Debug)]
pub struct DuplicateSceneSubmeshCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    physical_index: u32,
    catalog_default: EffectGraphDef,
    prev: Option<RemovedObjectSnapshot>,
    rejection: Option<String>,
}

impl DuplicateSceneSubmeshCommand {
    pub fn new(target: GraphTarget, render_scene_node_id: u32, physical_index: u32, catalog_default: EffectGraphDef) -> Self {
        Self { target, render_scene_node_id, physical_index, catalog_default, prev: None, rejection: None }
    }
}

impl Command for DuplicateSceneSubmeshCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) { targets.push(self.target.clone()); }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else { return; };
        let info = match compound_child_info(def, self.render_scene_node_id, self.physical_index) {
            Ok(info) => info,
            Err(reason) => { self.rejection = Some(reason.into()); return; }
        };
        let Some(instance) = resolve_target_instance(&self.target, project).map(|instance| InstanceLayerSnapshot::capture(instance)) else { return; };
        let render_id = self.render_scene_node_id;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let previous_metadata = def.preset_metadata.clone();
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut handles = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut handles);
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &[])?;
            let previous = (nodes.clone(), wires.clone());
            let current_objects = match nodes
                .iter()
                .find(|node| node.id == render_id)?
                .params
                .get("objects")
            {
                Some(SerializedParamValue::Float { value }) => *value as u32,
                Some(SerializedParamValue::Int { value }) => (*value).max(0) as u32,
                _ => return None,
            };
            let group_index = nodes.iter().position(|node| node.id == info.group_id)?;
            let mut group = nodes[group_index].group.as_deref()?.clone();
            let source_child_node_id = group
                .nodes
                .iter()
                .find(|node| node.id == info.child_id)
                .map(|node| node.node_id.clone())?;
            let owned = child_owned_ids(&group, info.child_id);
            let mut id_map = Vec::new();
            let mut clones = Vec::new();
            for source in group.nodes.iter().filter(|node| owned.contains(&node.id)) {
                let clone = deep_clone_with_fresh_ids(source, &mut next_id, &mut handles, &mut id_map);
                clones.push((source.id, clone));
            }
            let numeric_map: std::collections::HashMap<_, _> = clones.iter().map(|(old, clone)| (*old, clone.id)).collect();
            let mut cloned_wires = Vec::new();
            for wire in &group.wires {
                if let Some(&to_node) = numeric_map.get(&wire.to_node) {
                    let from_node = numeric_map.get(&wire.from_node).copied().unwrap_or(wire.from_node);
                    cloned_wires.push(EffectGraphWire { from_node, from_port: wire.from_port.clone(), to_node, to_port: wire.to_port.clone() });
                }
            }
            let new_child_id = *numeric_map.get(&info.child_id)?;
            let new_output_index = info.output_ports.len();
            let new_output_port = object_output_port(new_output_index);
            let new_output_id = next_id;
            group.nodes.extend(clones.into_iter().map(|(_, clone)| clone));
            group.nodes.push(scene_build_node(new_output_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new()));
            group.wires.extend(cloned_wires);
            group.wires.push(scene_build_wire(new_child_id, "object", new_output_id, &new_output_port));
            group.interface.outputs.push(InterfacePortDef { name: new_output_port.clone(), port_type: "Object".into() });
            sync_group_physics_compound(&mut group).ok()?;
            nodes[group_index].group = Some(Box::new(group));
            wires.push(scene_build_wire(info.group_id, &new_output_port, render_id, &format!("object_{current_objects}")));
            let render = nodes.iter_mut().find(|node| node.id == render_id)?;
            render.params.insert("objects".into(), SerializedParamValue::Float { value: current_objects as f32 + 1.0 });
            if let Some(meta) = def.preset_metadata.as_mut() {
                let source_bindings = meta.string_bindings.clone();
                for binding in source_bindings {
                    let target = match &binding.target {
                        BindingTarget::Node { node_id, param } => Some((node_id.clone(), param.clone())),
                        _ => None,
                    };
                    if let Some((node_id, param)) = target
                        && let Some((_, new_id)) = id_map.iter().find(|(old, _)| *old == node_id)
                    {
                        let mut cloned = binding;
                        cloned.target = BindingTarget::Node { node_id: new_id.clone(), param: param.clone() };
                        meta.string_bindings.push(cloned);
                    }
                }
            }
            clone_sections::clone_scene_bindings(def, &id_map);
            let cloned_child_node_id = id_map
                .iter()
                .find(|(old, _)| old == &source_child_node_id)
                .map(|(_, new)| new.clone())?;
            preserve_shared_parent_visible_binding(def, &source_child_node_id, &cloned_child_node_id);
            Some((previous, previous_metadata))
        });
        let Some((previous, metadata)) = result.flatten() else { self.rejection = Some("Submesh graph changed before duplication".into()); return; };
        self.prev = Some(RemovedObjectSnapshot { nodes: previous.0, wires: previous.1, metadata, instance });
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(snapshot) = self.prev.take() else { return; };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = snapshot.metadata;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &[]) { *nodes = snapshot.nodes; *wires = snapshot.wires; }
        });
        if let Some(instance) = resolve_target_instance(&self.target, project) { snapshot.instance.restore(instance); }
        refresh_target_manifest(project, &self.target);
    }
    fn description(&self) -> &str { "Duplicate Submesh" }
    fn was_applied(&self) -> bool { self.prev.is_some() }
    fn rejection_reason(&self) -> Option<&str> { self.rejection.as_deref() }
}

fn deletion_breaks_explicit_modifier_target(
    def: &EffectGraphDef,
    scope: &[u32],
    render_id: u32,
    object_index: u32,
) -> bool {
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    if def.scene_modifiers.is_empty() {
        return false;
    }
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    let mut path = Vec::with_capacity(scope.len());
    for id in scope {
        let Some(node) = nodes.iter().find(|node| node.id == *id) else {
            return false;
        };
        let Some(group) = node.group.as_deref() else {
            return false;
        };
        path.push(node.node_id.clone());
        nodes = &group.nodes;
        wires = &group.wires;
    }
    let port = format!("object_{object_index}");
    let Some(wire) = wires
        .iter()
        .find(|wire| wire.to_node == render_id && wire.to_port == port)
    else {
        return false;
    };
    let Some(producer) = nodes.iter().find(|node| node.id == wire.from_node) else {
        return false;
    };
    let removed = SceneNodeRef {
        scope: path,
        node: producer.node_id.clone(),
    };
    def.scene_modifiers
        .iter()
        .any(|modifier| match &modifier.targets {
            SceneTargetSelection::AllObjects => false,
            SceneTargetSelection::Explicit { objects } => objects.iter().any(|object| {
                object == &removed
                    || (object.scope.starts_with(&removed.scope)
                        && object.scope.get(removed.scope.len()) == Some(&removed.node))
            }),
        })
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.light_index;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                let light_port = format!("light_{k}");
                let light_id = wires
                    .iter()
                    .find(|w| w.to_node == render_id && w.to_port == light_port)
                    .map(|w| w.from_node)?;

                let current_lights = match nodes
                    .iter()
                    .find(|n| n.id == render_id)?
                    .params
                    .get("lights")
                {
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
            let inner = n
                .group
                .as_ref()
                .map(|g| max_node_id_over(&g.nodes))
                .unwrap_or(0);
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
        let remap = |id: u32| {
            id_map
                .iter()
                .find(|(o, _)| *o == id)
                .map(|(_, n)| *n)
                .unwrap_or(id)
        };
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
    wires
        .iter()
        .find(|w| w.to_node == render_id && w.to_port == object_port)
        .map(|w| w.from_node)
}

// Keep authoring in lock-step with the runtime world.  The graph schema is
// intentionally sparse (only occupied ports are serialized), so increasing
// this limit does not change existing documents.
const PHYSICS_BODY_SLOTS: u32 = 64;

#[derive(Debug, Clone)]
struct PhysicsSceneObject {
    world_id: u32,
    body_slot: u32,
    copies: bool,
    transform_id: u32,
    object_id: u32,
    owned_ids: Vec<u32>,
    grouped: bool,
    /// Every render slot fed by this producer, including compound material
    /// outputs from one imported group.
    render_indices: Vec<u32>,
}

#[derive(Debug, Clone)]
enum PhysicsSceneObjectMatch {
    NotPhysics,
    Valid(PhysicsSceneObject),
    Malformed(&'static str),
}

fn unique_input<'a>(
    wires: &'a [EffectGraphWire],
    to_node: u32,
    to_port: &str,
) -> Result<Option<&'a EffectGraphWire>, ()> {
    let mut matches = wires
        .iter()
        .filter(|wire| wire.to_node == to_node && wire.to_port == to_port);
    let first = matches.next();
    if matches.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

fn unique_output<'a>(
    wires: &'a [EffectGraphWire],
    from_node: u32,
    from_port: &str,
) -> Result<Option<&'a EffectGraphWire>, ()> {
    let mut matches = wires
        .iter()
        .filter(|wire| wire.from_node == from_node && wire.from_port == from_port);
    let first = matches.next();
    if matches.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

fn physics_scene_object_match(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    render_id: u32,
    object_index: u32,
    object_id: u32,
) -> PhysicsSceneObjectMatch {
    let Some(object) = nodes.iter().find(|node| node.id == object_id) else {
        return PhysicsSceneObjectMatch::Malformed("Physics object node is unavailable");
    };
    if object.type_id == GROUP_TYPE_ID && object.group.is_some() {
        return grouped_physics_scene_object_match(nodes, wires, render_id, object_index, object);
    }
    if object.type_id != "node.scene_object" {
        return PhysicsSceneObjectMatch::NotPhysics;
    }

    if physics_copies_candidate(nodes, wires, object_id) {
        return physics_copies_scene_object_match(nodes, wires, render_id, object_index, object_id);
    }

    let body_mesh = rigid_body_mesh_candidate(nodes, wires, object_id);

    let transform_wire = match unique_input(wires, object_id, "transform") {
        Ok(Some(wire)) => wire,
        Ok(None) if body_mesh => {
            return PhysicsSceneObjectMatch::Malformed("Physics object pose input is missing");
        }
        Ok(None) => return PhysicsSceneObjectMatch::NotPhysics,
        Err(()) => {
            return PhysicsSceneObjectMatch::Malformed(
                "Physics object transform input is duplicated",
            );
        }
    };
    let Some(world) = nodes
        .iter()
        .find(|node| node.id == transform_wire.from_node)
    else {
        return if body_mesh {
            PhysicsSceneObjectMatch::Malformed("Physics object pose world is unavailable")
        } else {
            PhysicsSceneObjectMatch::NotPhysics
        };
    };
    if world.type_id != "node.physics_world" {
        return if body_mesh {
            PhysicsSceneObjectMatch::Malformed("Physics object pose world is malformed")
        } else {
            PhysicsSceneObjectMatch::NotPhysics
        };
    }

    let Some(body_suffix) = transform_wire.from_port.strip_prefix("pose_") else {
        return PhysicsSceneObjectMatch::Malformed("Physics object pose port is malformed");
    };
    let Ok(body_slot) = body_suffix.parse::<u32>() else {
        return PhysicsSceneObjectMatch::Malformed("Physics object pose slot is malformed");
    };
    if body_slot >= PHYSICS_BODY_SLOTS {
        return PhysicsSceneObjectMatch::Malformed("Physics object pose slot is out of range");
    }
    let Ok(Some(pose_wire)) = unique_output(wires, world.id, transform_wire.from_port.as_str())
    else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object pose output is missing or shared",
        );
    };
    if pose_wire.to_node != object_id || pose_wire.to_port != "transform" {
        return PhysicsSceneObjectMatch::Malformed("Physics object pose output is malformed");
    }
    let body_port = format!("body_{body_slot}");
    let Ok(Some(body_wire)) = unique_input(wires, world.id, &body_port) else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object body input is missing or duplicated",
        );
    };
    if body_wire.from_port != "body" {
        return PhysicsSceneObjectMatch::Malformed("Physics object body output is malformed");
    }
    let Some(body) = nodes.iter().find(|node| node.id == body_wire.from_node) else {
        return PhysicsSceneObjectMatch::Malformed("Physics object body node is unavailable");
    };
    if body.type_id != "node.rigid_body" {
        return PhysicsSceneObjectMatch::Malformed("Physics object body node has the wrong type");
    }

    let Ok(Some(authored_transform_wire)) = unique_input(wires, body.id, "transform") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object authored transform is missing or duplicated",
        );
    };
    let Some(authored_transform) = nodes
        .iter()
        .find(|node| node.id == authored_transform_wire.from_node)
    else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object authored transform is unavailable",
        );
    };
    if authored_transform.type_id != "node.transform_3d"
        || authored_transform_wire.from_port != "transform"
    {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object authored transform is malformed",
        );
    }

    let Ok(Some(shape_wire)) = unique_output(wires, body.id, "shape") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object mesh shape input is missing or duplicated",
        );
    };
    let Some(mesh) = nodes.iter().find(|node| node.id == shape_wire.to_node) else {
        return PhysicsSceneObjectMatch::Malformed("Physics object mesh node is unavailable");
    };
    if mesh.type_id != "node.platonic_solid_mesh"
        || shape_wire.from_port != "shape"
        || shape_wire.to_port != "shape"
    {
        return PhysicsSceneObjectMatch::Malformed("Physics object mesh shape input is malformed");
    }

    let Ok(Some(vertices_wire)) = unique_input(wires, object_id, "vertices") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object mesh output is missing or duplicated",
        );
    };
    if vertices_wire.from_node != mesh.id || vertices_wire.from_port != "vertices" {
        return PhysicsSceneObjectMatch::Malformed("Physics object mesh output is malformed");
    }

    let Ok(Some(material_wire)) = unique_input(wires, object_id, "material") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object material input is missing or duplicated",
        );
    };
    let Some(material) = nodes.iter().find(|node| node.id == material_wire.from_node) else {
        return PhysicsSceneObjectMatch::Malformed("Physics object material node is unavailable");
    };
    if material.type_id != "node.pbr_material" || material_wire.from_port != "out" {
        return PhysicsSceneObjectMatch::Malformed("Physics object material input is malformed");
    }

    let Ok(Some(object_wire)) = unique_output(wires, object_id, "object") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics object render output is missing or duplicated",
        );
    };
    if object_wire.to_node != render_id || object_wire.to_port != format!("object_{object_index}") {
        return PhysicsSceneObjectMatch::Malformed("Physics object render output is malformed");
    }

    let owned_ids = vec![
        authored_transform.id,
        body.id,
        mesh.id,
        material.id,
        object.id,
    ];
    let owned = owned_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();

    // Owned outputs must stay exclusive to this object. External parameter
    // inputs (such as an LFO driving rotation) can be copied to a duplicate.
    let allowed = |wire: &EffectGraphWire| {
        let external_parameter_input = !owned.contains(&wire.from_node)
            && ((wire.to_node == authored_transform.id && wire.to_port != "transform")
                || (wire.to_node == body.id
                    && wire.to_port != "transform"
                    && wire.to_port != "shape"));
        (wire.from_node == authored_transform.id
            && wire.from_port == "transform"
            && wire.to_node == body.id
            && wire.to_port == "transform")
            || (wire.from_node == body.id
                && wire.from_port == "body"
                && wire.to_node == world.id
                && wire.to_port == body_port)
            || (wire.from_node == body.id
                && wire.from_port == "shape"
                && wire.to_node == mesh.id
                && wire.to_port == "shape")
            || (wire.from_node == mesh.id
                && wire.from_port == "vertices"
                && wire.to_node == object.id
                && wire.to_port == "vertices")
            || (wire.from_node == material.id
                && wire.from_port == "out"
                && wire.to_node == object.id
                && wire.to_port == "material")
            || (wire.from_node == world.id
                && wire.from_port == transform_wire.from_port
                && wire.to_node == object.id
                && wire.to_port == "transform")
            || (wire.from_node == object.id
                && wire.from_port == "object"
                && wire.to_node == render_id
                && wire.to_port == format!("object_{object_index}")
                || external_parameter_input)
    };
    if wires.iter().any(|wire| {
        (owned.contains(&wire.from_node) || owned.contains(&wire.to_node)) && !allowed(wire)
    }) {
        return PhysicsSceneObjectMatch::Malformed("Physics object chain is shared");
    }

    PhysicsSceneObjectMatch::Valid(PhysicsSceneObject {
        world_id: world.id,
        body_slot,
        copies: false,
        transform_id: authored_transform.id,
        object_id: object.id,
        owned_ids,
        grouped: false,
        render_indices: vec![object_index],
    })
}

fn grouped_physics_scene_object_match(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    render_id: u32,
    object_index: u32,
    group_node: &EffectGraphNode,
) -> PhysicsSceneObjectMatch {
    let Some(group) = group_node.group.as_deref() else {
        return PhysicsSceneObjectMatch::NotPhysics;
    };
    let Some(object) = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.scene_object")
    else {
        return PhysicsSceneObjectMatch::NotPhysics;
    };
    let Some(output) = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
    else {
        return PhysicsSceneObjectMatch::NotPhysics;
    };
    let Some(input) = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_INPUT_TYPE_ID)
    else {
        return PhysicsSceneObjectMatch::NotPhysics;
    };
    let Some(body_output) = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == "body")
    else {
        return PhysicsSceneObjectMatch::NotPhysics;
    };
    let Some(body) = group
        .nodes
        .iter()
        .find(|node| node.id == body_output.from_node && node.type_id == "node.rigid_body")
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group body node is unavailable");
    };
    let Some(_pose_wire) = group.wires.iter().find(|wire| {
        wire.from_node == input.id
            && wire.from_port == "pose"
            && wire.to_node == object.id
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
    }) else {
        return PhysicsSceneObjectMatch::Malformed("Physics group pose input is malformed");
    };
    let Some(authored_wire) = group
        .wires
        .iter()
        .find(|wire| wire.to_node == body.id && wire.to_port == "transform")
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group authored transform is missing");
    };
    let Some(authored) = group
        .nodes
        .iter()
        .find(|node| node.id == authored_wire.from_node && node.type_id == "node.transform_3d")
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group authored transform is malformed");
    };
    let Some(object_wire) = wires.iter().find(|wire| {
        wire.from_node == group_node.id
            && wire.from_port == "object"
            && wire.to_node == render_id
            && wire.to_port == format!("object_{object_index}")
    }) else {
        return PhysicsSceneObjectMatch::Malformed("Physics group render output is malformed");
    };
    let _ = object_wire;
    let Some(body_wire) = wires
        .iter()
        .find(|wire| wire.from_node == group_node.id && wire.from_port == "body")
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group body output is missing");
    };
    let Some(world) = nodes
        .iter()
        .find(|node| node.id == body_wire.to_node && node.type_id == "node.physics_world")
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group world is unavailable");
    };
    let Some(body_slot) = body_wire
        .to_port
        .strip_prefix("body_")
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return PhysicsSceneObjectMatch::Malformed("Physics group body slot is malformed");
    };
    if body_slot >= PHYSICS_BODY_SLOTS {
        return PhysicsSceneObjectMatch::Malformed("Physics group body slot is out of range");
    }
    let compound_count = group
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_object")
        .count();
    if compound_count > PHYSICS_BODY_SLOTS as usize {
        return PhysicsSceneObjectMatch::Malformed("Physics compound group has more than 64 parts");
    }
    for part in 0..compound_count {
        if !group.wires.iter().any(|wire| {
            wire.to_node == body.id
                && wire.to_port == "part_".to_string() + &part.to_string()
                && wire.from_port == "transform"
        }) {
            return PhysicsSceneObjectMatch::Malformed("Physics compound child transform is missing");
        }
    }
    let Some(pose_root_wire) = wires.iter().find(|wire| {
        wire.from_node == world.id
            && wire.from_port == format!("pose_{body_slot}")
            && wire.to_node == group_node.id
            && wire.to_port == "pose"
    }) else {
        return PhysicsSceneObjectMatch::Malformed("Physics group pose output is missing");
    };
    let _ = pose_root_wire;
    let body_port = format!("body_{body_slot}");
    if wires
        .iter()
        .filter(|wire| wire.to_node == world.id && wire.to_port == body_port)
        .count()
        != 1
    {
        return PhysicsSceneObjectMatch::Malformed("Physics group body slot is duplicated");
    }
    PhysicsSceneObjectMatch::Valid(PhysicsSceneObject {
        world_id: world.id,
        body_slot,
        copies: false,
        transform_id: authored.id,
        object_id: group_node.id,
        owned_ids: vec![group_node.id],
        grouped: true,
        render_indices: group_render_indices(wires, render_id, group_node.id),
    })
}

fn group_render_indices(
    wires: &[EffectGraphWire],
    render_id: u32,
    group_id: u32,
) -> Vec<u32> {
    let mut indices: Vec<u32> = wires
        .iter()
        .filter_map(|wire| {
            if wire.from_node != group_id || wire.to_node != render_id {
                return None;
            }
            wire.to_port
                .strip_prefix("object_")
                .and_then(|value| value.parse::<u32>().ok())
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Detect the shipped Physics Boxes `copies` shape before ordinary pose-slot
/// matching. This stays deliberately local to the scene object and its direct
/// producers, so a partially edited copies chain is rejected atomically.
fn physics_copies_candidate(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    object_id: u32,
) -> bool {
    let world_input = |wire: &EffectGraphWire| {
        (wire.to_port == "instances" || wire.to_port == "instance_count")
            && wire.to_node == object_id
            && nodes
                .iter()
                .any(|node| node.id == wire.from_node && node.type_id == "node.physics_world")
    };
    if wires.iter().any(world_input) {
        return true;
    }

    let Some(vertices_wire) = wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "vertices")
    else {
        return false;
    };
    let Some(mesh) = nodes.iter().find(|node| node.id == vertices_wire.from_node) else {
        return false;
    };
    let has_shape_body = wires.iter().any(|shape_wire| {
        shape_wire.to_node == mesh.id
            && shape_wire.to_port == "shape"
            && shape_wire.from_port == "shape"
            && nodes.iter().any(|node| {
                node.id == shape_wire.from_node
                    && node.type_id == "node.rigid_body"
                    && wires.iter().any(|body_wire| {
                        body_wire.from_node == node.id
                            && body_wire.from_port == "body"
                            && body_wire.to_port == "copies"
                            && nodes.iter().any(|world| {
                                world.id == body_wire.to_node
                                    && world.type_id == "node.physics_world"
                            })
                    })
            })
    });
    if has_shape_body {
        return true;
    }

    false
}

fn rigid_body_mesh_candidate(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    object_id: u32,
) -> bool {
    wires.iter().any(|wire| {
        wire.to_node == object_id
            && wire.to_port == "vertices"
            && nodes.iter().any(|mesh| {
                mesh.id == wire.from_node
                    && mesh.type_id == "node.platonic_solid_mesh"
                    && wires.iter().any(|shape| {
                        shape.to_node == mesh.id
                            && shape.to_port == "shape"
                            && nodes.iter().any(|body| {
                                body.id == shape.from_node && body.type_id == "node.rigid_body"
                            })
                    })
            })
    })
}

fn physics_copies_scene_object_match(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    render_id: u32,
    object_index: u32,
    object_id: u32,
) -> PhysicsSceneObjectMatch {
    let object = nodes
        .iter()
        .find(|node| node.id == object_id)
        .expect("copies candidate has an object node");

    let Ok(Some(vertices_wire)) = unique_input(wires, object_id, "vertices") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh output is missing or duplicated",
        );
    };
    if vertices_wire.from_port != "vertices" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh output is malformed",
        );
    }
    let Some(mesh) = nodes.iter().find(|node| node.id == vertices_wire.from_node) else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh node is unavailable",
        );
    };
    if mesh.type_id != "node.platonic_solid_mesh" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh node has the wrong type",
        );
    }

    let Ok(Some(shape_wire)) = unique_input(wires, mesh.id, "shape") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh shape input is missing or duplicated",
        );
    };
    if shape_wire.from_port != "shape" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object mesh shape input is malformed",
        );
    }
    let Some(body) = nodes.iter().find(|node| node.id == shape_wire.from_node) else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object body node is unavailable",
        );
    };
    if body.type_id != "node.rigid_body" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object body node has the wrong type",
        );
    }

    let Ok(Some(body_wire)) = unique_output(wires, body.id, "body") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object body output is missing or duplicated",
        );
    };
    if body_wire.to_port != "copies" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object body output is malformed",
        );
    }
    let Some(world) = nodes.iter().find(|node| node.id == body_wire.to_node) else {
        return PhysicsSceneObjectMatch::Malformed("Physics copies object world is unavailable");
    };
    if world.type_id != "node.physics_world" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object world has the wrong type",
        );
    }
    let Ok(Some(copies_wire)) = unique_input(wires, world.id, "copies") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies world input is missing or duplicated",
        );
    };
    if copies_wire.from_node != body.id || copies_wire.from_port != "body" {
        return PhysicsSceneObjectMatch::Malformed("Physics copies world input is malformed");
    }

    let Ok(Some(authored_transform_wire)) = unique_input(wires, body.id, "transform") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object authored transform is missing or duplicated",
        );
    };
    let Some(authored_transform) = nodes
        .iter()
        .find(|node| node.id == authored_transform_wire.from_node)
    else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object authored transform is unavailable",
        );
    };
    if authored_transform.type_id != "node.transform_3d"
        || authored_transform_wire.from_port != "transform"
    {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object authored transform is malformed",
        );
    }

    let Ok(Some(material_wire)) = unique_input(wires, object_id, "material") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object material input is missing or duplicated",
        );
    };
    let Some(material) = nodes.iter().find(|node| node.id == material_wire.from_node) else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object material node is unavailable",
        );
    };
    if material.type_id != "node.pbr_material" || material_wire.from_port != "out" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object material input is malformed",
        );
    }

    let Ok(Some(instances_wire)) = unique_input(wires, object_id, "instances") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object instances input is missing or duplicated",
        );
    };
    if instances_wire.from_node != world.id || instances_wire.from_port != "instances" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object instances input is malformed",
        );
    }
    let Ok(Some(world_instances_wire)) = unique_output(wires, world.id, "instances") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies world instances output is missing or shared",
        );
    };
    if world_instances_wire.to_node != object_id || world_instances_wire.to_port != "instances" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies world instances output is malformed",
        );
    }

    let Ok(Some(count_wire)) = unique_input(wires, object_id, "instance_count") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object count input is missing or duplicated",
        );
    };
    if count_wire.from_node != world.id || count_wire.from_port != "active_count" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object count input is malformed",
        );
    }
    let Ok(Some(world_count_wire)) = unique_output(wires, world.id, "active_count") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies world count output is missing or shared",
        );
    };
    if world_count_wire.to_node != object_id || world_count_wire.to_port != "instance_count" {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies world count output is malformed",
        );
    }

    let Ok(Some(object_wire)) = unique_output(wires, object_id, "object") else {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object render output is missing or duplicated",
        );
    };
    if object_wire.to_node != render_id || object_wire.to_port != format!("object_{object_index}") {
        return PhysicsSceneObjectMatch::Malformed(
            "Physics copies object render output is malformed",
        );
    }

    let owned_ids = vec![
        authored_transform.id,
        body.id,
        mesh.id,
        material.id,
        object.id,
    ];
    let owned = owned_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let allowed = |wire: &EffectGraphWire| {
        let external_parameter_input = !owned.contains(&wire.from_node)
            && ((wire.to_node == authored_transform.id && wire.to_port != "transform")
                || (wire.to_node == body.id
                    && wire.to_port != "transform"
                    && wire.to_port != "shape"));
        (wire.from_node == authored_transform.id
            && wire.from_port == "transform"
            && wire.to_node == body.id
            && wire.to_port == "transform")
            || (wire.from_node == body.id
                && wire.from_port == "body"
                && wire.to_node == world.id
                && wire.to_port == "copies")
            || (wire.from_node == body.id
                && wire.from_port == "shape"
                && wire.to_node == mesh.id
                && wire.to_port == "shape")
            || (wire.from_node == mesh.id
                && wire.from_port == "vertices"
                && wire.to_node == object.id
                && wire.to_port == "vertices")
            || (wire.from_node == material.id
                && wire.from_port == "out"
                && wire.to_node == object.id
                && wire.to_port == "material")
            || (wire.from_node == world.id
                && wire.from_port == "instances"
                && wire.to_node == object.id
                && wire.to_port == "instances")
            || (wire.from_node == world.id
                && wire.from_port == "active_count"
                && wire.to_node == object.id
                && wire.to_port == "instance_count")
            || (wire.from_node == object.id
                && wire.from_port == "object"
                && wire.to_node == render_id
                && wire.to_port == format!("object_{object_index}"))
            || external_parameter_input
    };
    if wires.iter().any(|wire| {
        (owned.contains(&wire.from_node) || owned.contains(&wire.to_node)) && !allowed(wire)
    }) {
        return PhysicsSceneObjectMatch::Malformed("Physics copies object chain is shared");
    }

    PhysicsSceneObjectMatch::Valid(PhysicsSceneObject {
        world_id: world.id,
        body_slot: 0,
        copies: true,
        transform_id: authored_transform.id,
        object_id: object.id,
        owned_ids,
        grouped: false,
        render_indices: vec![object_index],
    })
}

fn first_free_physics_body_slot(wires: &[EffectGraphWire], world_id: u32) -> Option<u32> {
    (0..PHYSICS_BODY_SLOTS).find(|slot| {
        let body_port = format!("body_{slot}");
        let pose_port = format!("pose_{slot}");
        !wires.iter().any(|wire| {
            (wire.to_node == world_id && wire.to_port == body_port)
                || (wire.from_node == world_id && wire.from_port == pose_port)
        })
    })
}

/// The source and authored-transform facts needed by the standard imported
/// object authoring commands.  Keeping this discovery local to editing is
/// deliberate: the renderer VM is a read model, while commands must validate
/// the graph again on the content thread before changing it.
#[derive(Debug, Clone)]
struct ImportedPhysicsSource {
    node_id: NodeId,
    params: BTreeMap<String, SerializedParamValue>,
    scope_is_group: bool,
}

#[derive(Debug, Clone)]
struct ImportedObjectParts {
    producer_id: u32,
    object_id: u32,
    group_id: Option<u32>,
    authored_transform_id: u32,
    source: ImportedPhysicsSource,
    /// Every retained static compound source in render order.  The first
    /// entry is also `source`; keeping the complete list lets the rigid body
    /// author a stable `compound_materials` selector table instead of
    /// collapsing a multi-material asset to the primary material.
    compound_sources: Vec<ImportedPhysicsSource>,
    object_handle: String,
    render_indices: Vec<u32>,
}

const IMPORTED_SOURCE_PARAMS: &[&str] = &[
    "path",
    "mesh_index",
    "primitive_index",
    "material_index",
    "fit",
    "recenter",
    "translate_x",
    "translate_y",
    "translate_z",
    "fragment_count",
    "fragment_index",
];

fn source_param_default(name: &str) -> SerializedParamValue {
    match name {
        "path" => SerializedParamValue::String {
            value: String::new(),
        },
        "fit" => SerializedParamValue::Enum { value: 0 },
        "recenter" => SerializedParamValue::Bool { value: true },
        "mesh_index" | "primitive_index" | "material_index" => {
            SerializedParamValue::Int { value: -1 }
        }
        "fragment_count" => SerializedParamValue::Int { value: 1 },
        "fragment_index" => SerializedParamValue::Int { value: 0 },
        _ if name.starts_with("translate_") => SerializedParamValue::Float { value: 0.0 },
        _ => SerializedParamValue::Float { value: 0.0 },
    }
}

fn object_node_in_group(group: &GroupDef) -> Option<u32> {
    let output = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)?;
    let object_wire = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == "object")?;
    let object = group
        .nodes
        .iter()
        .find(|node| node.id == object_wire.from_node)?;
    (object.type_id == "node.scene_object").then_some(object.id)
}

fn object_node_for_group_output(group: &GroupDef, output_port: &str) -> Option<u32> {
    let object_wire = group
        .wires
        .iter()
        .find(|wire| {
            wire.to_port == output_port
                && group
                    .nodes
                    .iter()
                    .any(|node| node.id == wire.to_node && node.type_id == GROUP_OUTPUT_TYPE_ID)
        })?;
    let object = group
        .nodes
        .iter()
        .find(|node| node.id == object_wire.from_node)?;
    (object.type_id == "node.scene_object").then_some(object.id)
}

fn group_output_port_for_render_wire(wire: &EffectGraphWire) -> Option<&str> {
    (wire.from_port == "object" || wire.from_port.starts_with("object_")).then_some(wire.from_port.as_str())
}

fn authored_transform_in_level(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    object_id: u32,
) -> Result<u32, &'static str> {
    let Some(wire) = wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "transform")
    else {
        return Err("Enable Physics requires an authored transform");
    };
    let node = nodes
        .iter()
        .find(|node| node.id == wire.from_node)
        .ok_or("Enable Physics authored transform is unavailable")?;
    if node.type_id == "node.transform_3d" && wire.from_port == "transform" {
        return Ok(node.id);
    }
    let body_id = if node.type_id == GROUP_INPUT_TYPE_ID && wire.from_port == "pose" {
        let output = nodes
            .iter()
            .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
            .ok_or("Missing group output")?;
        wires
            .iter()
            .find(|w| w.to_node == output.id && w.to_port == "body")
            .map(|w| w.from_node)
    } else if node.type_id == "node.physics_world" {
        wire.from_port.strip_prefix("pose_").and_then(|slot| {
            wires
                .iter()
                .find(|w| w.to_node == node.id && w.to_port == format!("body_{slot}"))
                .map(|w| w.from_node)
        })
    } else {
        None
    }
    .ok_or("Enable Physics supports a direct authored transform only")?;
    if !nodes
        .iter()
        .any(|n| n.id == body_id && n.type_id == "node.rigid_body")
    {
        return Err("Invalid physics body");
    }
    let source = wires
        .iter()
        .find(|w| w.to_node == body_id && w.to_port == "transform")
        .ok_or("Missing body transform")?;
    nodes
        .iter()
        .find(|n| n.id == source.from_node && n.type_id == "node.transform_3d")
        .map(|n| n.id)
        .ok_or("Physics needs a direct authored transform")
}

/// Resolve the shared parent transform of a compound group.  New compound
/// imports feed every child scene object through `parent_transform`; older
/// graphs used the same transform node directly on `transform`, so retain
/// that shape as a compatibility fallback for undoable edits.
fn group_authored_transform_in_level(
    group: &GroupDef,
    object_id: u32,
) -> Result<u32, &'static str> {
    let wire = group
        .wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "parent_transform")
        .or_else(|| {
            group
                .wires
                .iter()
                .find(|wire| wire.to_node == object_id && wire.to_port == "transform")
        })
        .ok_or("Enable Physics requires a shared group transform")?;
    let node = group
        .nodes
        .iter()
        .find(|node| node.id == wire.from_node)
        .ok_or("Enable Physics shared group transform is unavailable")?;
    if node.type_id == "node.transform_3d" && wire.from_port == "transform" {
        return Ok(node.id);
    }
    if node.type_id == GROUP_INPUT_TYPE_ID && wire.from_port == "pose" {
        let body_output = group
            .nodes
            .iter()
            .find(|candidate| candidate.type_id == GROUP_OUTPUT_TYPE_ID)
            .and_then(|output| {
                group
                    .wires
                    .iter()
                    .find(|candidate| candidate.to_node == output.id && candidate.to_port == "body")
                    .map(|candidate| candidate.from_node)
            })
            .ok_or("Enable Physics group body output is unavailable")?;
        let body_transform = group
            .wires
            .iter()
            .find(|candidate| candidate.to_node == body_output && candidate.to_port == "transform")
            .ok_or("Enable Physics group body transform is unavailable")?;
        return group
            .nodes
            .iter()
            .find(|candidate| candidate.id == body_transform.from_node && candidate.type_id == "node.transform_3d")
            .map(|candidate| candidate.id)
            .ok_or("Enable Physics group body transform is malformed");
    }
    Err("Enable Physics requires a direct shared group transform")
}

/// Follow the scene object's mesh input through the curated single-mesh
/// modifiers and transparent groups until its glTF source.  Skinned and
/// otherwise GPU-deformed sources are rejected before mutation because their
/// rendered geometry is not a stable standard Box3D collider source.
fn imported_source_in_level(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    object_id: u32,
) -> Result<ImportedPhysicsSource, &'static str> {
    let mut current_nodes = nodes;
    let mut cursor = wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "vertices")
        .map(|wire| (wire.from_node, wire.from_port.as_str()));
    let mut scope_is_group = false;
    let mut guard = 0;
    while let Some((node_id, port)) = cursor {
        guard += 1;
        if guard > 64 {
            return Err("Enable Physics rejected a cyclic mesh source");
        }
        let node = current_nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or("Enable Physics mesh source is unavailable")?;
        if node.type_id == GROUP_TYPE_ID {
            let group = node
                .group
                .as_deref()
                .ok_or("Enable Physics rejected a malformed mesh group")?;
            let output = group
                .nodes
                .iter()
                .find(|inner| inner.type_id == GROUP_OUTPUT_TYPE_ID)
                .ok_or("Enable Physics mesh group has no output")?;
            let wire = group
                .wires
                .iter()
                .find(|wire| wire.to_node == output.id && wire.to_port == port)
                .ok_or("Enable Physics mesh group output is unwired")?;
            current_nodes = &group.nodes;
            cursor = Some((wire.from_node, wire.from_port.as_str()));
            scope_is_group = true;
            continue;
        }
        match node.type_id.as_str() {
            "node.gltf_mesh_source" => {
                return Ok(ImportedPhysicsSource {
                    node_id: node.node_id.clone(),
                    params: node.params.clone(),
                    scope_is_group,
                });
            }
            "node.gltf_skinned_mesh_source"
            | "node.skin_mesh"
            | "node.morph_targets_blend"
            | "node.gltf_morph_deltas_source" => {
                return Err("Enable Physics does not support skinned or GPU-deformed sources");
            }
            _ => return Err("Enable Physics requires a supported glTF mesh source"),
        }
    }
    Err("Enable Physics requires a supported glTF mesh source")
}

fn imported_object_parts(
    def: &EffectGraphDef,
    render_id: u32,
    object_index: u32,
) -> Result<ImportedObjectParts, &'static str> {
    let producer_id = object_producer_id(&def.wires, render_id, object_index)
        .ok_or("Selected scene object is unavailable")?;
    let producer = def
        .nodes
        .iter()
        .find(|node| node.id == producer_id)
        .ok_or("Selected scene object producer is unavailable")?;
    if producer.type_id == GROUP_TYPE_ID {
        let group = producer
            .group
            .as_deref()
            .ok_or("Selected scene object group is malformed")?;
        let outer_wire = def
            .wires
            .iter()
            .find(|wire| wire.to_node == render_id && wire.to_port == format!("object_{object_index}"))
            .ok_or("Selected scene object render output is unavailable")?;
        let output_port = group_output_port_for_render_wire(outer_wire)
            .ok_or("Selected scene object group output is malformed")?;
        let object_id = object_node_for_group_output(group, output_port)
            .ok_or("Selected scene object group has no scene_object output")?;
        let authored_transform_id =
            group_authored_transform_in_level(group, object_id)?;
        let source = imported_source_in_level(&group.nodes, &group.wires, object_id)?;
        let mut compound_sources = Vec::new();
        for port in group.interface.outputs.iter().filter(|port| port.port_type == "Object") {
            let Some(part_id) = object_node_for_group_output(group, &port.name) else {
                return Err("Selected scene object group has an unsupported material or mesh chain");
            };
            compound_sources.push(imported_source_in_level(&group.nodes, &group.wires, part_id)?);
        }
        if compound_sources.is_empty() {
            return Err("Selected scene object group has no material sources");
        }
        let mut source = source;
        source.scope_is_group = true;
        let object = group
            .nodes
            .iter()
            .find(|node| node.id == object_id)
            .ok_or("Selected scene object is unavailable")?;
        return Ok(ImportedObjectParts {
            producer_id,
            object_id,
            group_id: Some(producer_id),
            authored_transform_id,
            source,
            compound_sources,
            object_handle: object
                .handle
                .clone()
                .or_else(|| producer.handle.clone())
                .unwrap_or_else(|| format!("Object {object_index}")),
            render_indices: group_render_indices(&def.wires, render_id, producer_id),
        });
    }
    if producer.type_id != "node.scene_object" {
        return Err("Selected scene object is a custom graph source");
    }
    let authored_transform_id = authored_transform_in_level(&def.nodes, &def.wires, producer_id)?;
    let source = imported_source_in_level(&def.nodes, &def.wires, producer_id)?;
    let compound_sources = vec![source.clone()];
    Ok(ImportedObjectParts {
        producer_id,
        object_id: producer_id,
        group_id: None,
        authored_transform_id,
        source,
        compound_sources,
        object_handle: producer
            .handle
            .clone()
            .unwrap_or_else(|| format!("Object {object_index}")),
        render_indices: vec![object_index],
    })
}

fn imported_body_params(
    source: &ImportedPhysicsSource,
    def: &EffectGraphDef,
) -> Result<BTreeMap<String, SerializedParamValue>, &'static str> {
    let mut params = BTreeMap::new();
    for name in IMPORTED_SOURCE_PARAMS {
        let value = source
            .params
            .get(*name)
            .cloned()
            .or_else(|| {
                if *name == "path" {
                    def.preset_metadata.as_ref().and_then(|meta| {
                        meta.string_bindings
                            .iter()
                            .find_map(|binding| match &binding.target {
                                BindingTarget::Node { node_id, param }
                                    if node_id == &source.node_id && param == "path" =>
                                {
                                    Some(SerializedParamValue::String {
                                        value: binding.default_value.clone(),
                                    })
                                }
                                _ => None,
                            })
                    })
                } else {
                    None
                }
            })
            .unwrap_or_else(|| source_param_default(name));
        if *name == "path"
            && matches!(&value, SerializedParamValue::String { value } if value.is_empty())
        {
            return Err("Enable Physics requires a bound glTF source path");
        }
        params.insert((*name).to_string(), value);
    }
    params.insert(
        "collider_parts".to_string(),
        SerializedParamValue::Int { value: 32 },
    );
    Ok(params)
}

fn compound_materials_param(
    sources: &[ImportedPhysicsSource],
) -> Result<SerializedParamValue, &'static str> {
    if sources.len() > PHYSICS_BODY_SLOTS as usize {
        return Err("Physics compound objects support at most 64 material parts");
    }
    let rows = sources
        .iter()
        .enumerate()
        .map(|(slot, source)| {
            let material_index = match source.params.get("material_index") {
                Some(SerializedParamValue::Int { value }) => *value as f32,
                Some(SerializedParamValue::Float { value }) => *value,
                _ => -1.0,
            };
            vec![slot as f32, material_index]
        })
        .collect();
    Ok(SerializedParamValue::Table { rows })
}

fn source_string_binding(
    def: &EffectGraphDef,
    source: &ImportedPhysicsSource,
    body_node_id: NodeId,
) -> Option<StringBindingDef> {
    def.preset_metadata
        .as_ref()?
        .string_bindings
        .iter()
        .find_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param }
                if node_id == &source.node_id && param == "path" =>
            {
                Some(StringBindingDef {
                    id: binding.id.clone(),
                    label: binding.label.clone(),
                    default_value: binding.default_value.clone(),
                    target: BindingTarget::Node {
                        node_id: body_node_id.clone(),
                        param: "path".to_string(),
                    },
                })
            }
            _ => None,
        })
}

fn fresh_scene_node(
    id: u32,
    type_id: &str,
    handle: Option<String>,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
    scene_build_node(id, type_id, handle, params)
}

fn add_group_physics(
    group: &mut GroupDef,
    body_id: u32,
    body_params: BTreeMap<String, SerializedParamValue>,
    body_handle: String,
    input_id: u32,
    _output_id: u32,
    authored_transform_id: u32,
    object_id: u32,
) -> Result<(NodeId, NodeId), &'static str> {
    let output_exists = group
        .nodes
        .iter()
        .any(|node| node.type_id == GROUP_OUTPUT_TYPE_ID);
    if !output_exists {
        return Err("Physics group has no output boundary");
    }
    if group.interface.inputs.iter().any(|p| p.name == "pose")
        || group.interface.outputs.iter().any(|p| p.name == "body")
    {
        return Err("Physics group already has a body interface");
    }
    let object_transform_wire = group
        .wires
        .iter()
        .position(|wire| wire.to_node == object_id && wire.to_port == "parent_transform")
        .or_else(|| {
            group
                .wires
                .iter()
                .position(|wire| wire.to_node == object_id && wire.to_port == "transform")
        })
        .ok_or("Physics group scene_object parent transform is unwired")?;
    if group.wires[object_transform_wire].from_node != authored_transform_id {
        return Err("Physics group scene_object parent transform is already driven");
    }
    let body = fresh_scene_node(body_id, "node.rigid_body", Some(body_handle), body_params);
    let body_node_id = body.node_id.clone();
    let input = fresh_scene_node(input_id, GROUP_INPUT_TYPE_ID, None, BTreeMap::new());
    let input_node_id = input.node_id.clone();
    let output_id = group
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
        .unwrap()
        .id;
    group.nodes.push(body);
    group.nodes.push(input);
    group.interface.inputs.push(InterfacePortDef {
        name: "pose".to_string(),
        port_type: "Transform".to_string(),
    });
    group.interface.outputs.push(InterfacePortDef {
        name: "body".to_string(),
        port_type: "RigidBody".to_string(),
    });
    let child_transform_ids: Vec<u32> = group
        .interface
        .outputs
        .iter()
        .filter(|port| port.port_type == "Object")
        .filter_map(|port| {
            let child_id = object_node_for_group_output(group, &port.name)?;
            group
                .wires
                .iter()
                .find(|wire| wire.to_node == child_id && wire.to_port == "transform")
                .map(|wire| wire.from_node)
        })
        .collect();
    let object_ids: std::collections::HashSet<_> = group.nodes.iter()
        .filter(|node| node.type_id == "node.scene_object").map(|node| node.id).collect();
    for wire in &mut group.wires {
        if object_ids.contains(&wire.to_node)
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
            && wire.from_node == authored_transform_id
        {
            wire.from_node = input_id;
            wire.from_port = "pose".into();
        }
    }
    group.wires.push(scene_build_wire(
        authored_transform_id,
        "transform",
        body_id,
        "transform",
    ));
    group
        .wires
        .push(scene_build_wire(body_id, "body", output_id, "body"));
    // A compound body keeps the asset-wide transform on `transform`, while
    // each retained material part contributes its own local transform on a
    // dedicated `part_N` input.  The renderer uses these inputs when it
    // prepares standard Box3D compound hulls.
    for (part_index, local_id) in child_transform_ids.into_iter().enumerate() {
        let Some(local) = group.nodes.iter().find(|node| node.id == local_id) else {
            return Err("Physics compound child transform is unavailable");
        };
        if local.type_id != "node.transform_3d" {
            return Err("Physics compound child transform is malformed");
        }
        group.wires.push(scene_build_wire(
            local.id,
            "transform",
            body_id,
            &format!("part_{part_index}"),
        ));
    }
    // Preserve the existing object output boundary wire; only the new body
    // output is added here.
    Ok((body_node_id, input_node_id))
}

fn remove_group_physics(
    group: &mut GroupDef,
    body_id: u32,
    object_id: u32,
    authored_transform_id: u32,
) -> Result<(), &'static str> {
    let input_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_INPUT_TYPE_ID)
        .ok_or("Physics group pose input is unavailable")?
        .id;
    let output_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .ok_or("Physics group body output is unavailable")?
        .id;
    if !group.wires.iter().any(|wire| {
        wire.from_node == input_id
            && wire.from_port == "pose"
            && wire.to_node == object_id
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
    }) {
        return Err("Physics group pose input is malformed");
    }
    if !group.wires.iter().any(|wire| {
        wire.from_node == body_id
            && wire.from_port == "body"
            && wire.to_node == output_id
            && wire.to_port == "body"
    }) {
        return Err("Physics group body output is malformed");
    }
    for wire in &mut group.wires {
        if wire.from_node == input_id
            && wire.from_port == "pose"
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
        {
            wire.from_node = authored_transform_id;
            wire.from_port = "transform".into();
        }
    }
    group.wires.retain(|wire| {
        !((wire.from_node == body_id && wire.to_node == output_id)
            || (wire.from_node == authored_transform_id && wire.to_node == body_id)
            || (wire.to_node == body_id && wire.to_port.starts_with("part_")))
    });
    group
        .nodes
        .retain(|node| node.id != body_id && node.id != input_id);
    // Remove the boundary only when it has no other non-object output; the
    // imported object shape has exactly one output and this keeps malformed
    // hand-authored groups from losing unrelated ports.
    let body_output_used = group
        .wires
        .iter()
        .any(|wire| wire.to_node == output_id && wire.to_port == "body");
    if !body_output_used {
        group.interface.inputs.retain(|port| port.name != "pose");
        group.interface.outputs.retain(|port| port.name != "body");
        let output_still_used = group.wires.iter().any(|wire| wire.to_node == output_id);
        if !output_still_used {
            group.nodes.retain(|node| node.id != output_id);
        }
    }
    Ok(())
}

fn strip_group_physics_for_split(group: &mut GroupDef) -> Result<(), &'static str> {
    let body_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.rigid_body")
        .map(|node| node.id)
        .ok_or("Split Physics group body is unavailable")?;
    if !group
        .nodes
        .iter()
        .any(|node| node.type_id == GROUP_INPUT_TYPE_ID)
    {
        return Err("Split Physics group pose input is unavailable");
    }
    let object_id =
        object_node_in_group(group).ok_or("Split Physics group object is unavailable")?;
    let authored_transform_id = group
        .wires
        .iter()
        .find(|wire| wire.to_node == body_id && wire.to_port == "transform")
        .map(|wire| wire.from_node)
        .ok_or("Split Physics authored transform is unavailable")?;
    remove_group_physics(group, body_id, object_id, authored_transform_id)
}

#[derive(Debug, Clone)]
struct ImportedPhysicsBinding {
    world_id: u32,
    body_slot: u32,
    body_id: u32,
}

fn imported_physics_binding(
    def: &EffectGraphDef,
    parts: &ImportedObjectParts,
) -> Result<ImportedPhysicsBinding, &'static str> {
    let Some(group_id) = parts.group_id else {
        let pose_wire = def
            .wires
            .iter()
            .find(|wire| wire.to_node == parts.object_id && wire.to_port == "transform")
            .ok_or("Physics object pose input is missing")?;
        let world = def
            .nodes
            .iter()
            .find(|node| node.id == pose_wire.from_node)
            .ok_or("Physics object world is unavailable")?;
        if world.type_id != "node.physics_world" {
            return Err("Selected object does not have standard physics enabled");
        }
        let slot = pose_wire
            .from_port
            .strip_prefix("pose_")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|slot| *slot < PHYSICS_BODY_SLOTS)
            .ok_or("Physics object pose slot is malformed")?;
        let body_wire = def
            .wires
            .iter()
            .find(|wire| wire.to_node == world.id && wire.to_port == format!("body_{slot}"))
            .ok_or("Physics object body input is missing")?;
        let body = def
            .nodes
            .iter()
            .find(|node| node.id == body_wire.from_node)
            .ok_or("Physics object body is unavailable")?;
        if body.type_id != "node.rigid_body" {
            return Err("Physics object body has the wrong type");
        }
        return Ok(ImportedPhysicsBinding {
            world_id: world.id,
            body_slot: slot,
            body_id: body.id,
        });
    };

    let group = def
        .nodes
        .iter()
        .find(|node| node.id == group_id)
        .and_then(|node| node.group.as_deref())
        .ok_or("Physics object group is unavailable")?;
    let output = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .ok_or("Physics object group output is unavailable")?;
    if !group
        .wires
        .iter()
        .any(|wire| wire.to_node == output.id && wire.to_port == "body")
    {
        return Err("Physics object group body output is missing");
    }
    // The body output boundary is fed by the body node, so resolve that
    // direction explicitly rather than trusting a hand-authored port name.
    let body_id = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == "body")
        .map(|wire| wire.from_node)
        .ok_or("Physics object group body output is malformed")?;
    let body = group
        .nodes
        .iter()
        .find(|node| node.id == body_id)
        .ok_or("Physics object group body is unavailable")?;
    if body.type_id != "node.rigid_body" {
        return Err("Physics object group body has the wrong type");
    }
    // Top-level world pose is the producer, and the group is its consumer.
    let (world_id, slot) = {
        let pose_source = def
            .wires
            .iter()
            .find(|wire| wire.to_node == parts.producer_id && wire.to_port == "pose")
            .ok_or("Physics object group pose input is missing")?;
        let world = def
            .nodes
            .iter()
            .find(|node| node.id == pose_source.from_node)
            .ok_or("Physics object group world is unavailable")?;
        let slot = pose_source
            .from_port
            .strip_prefix("pose_")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|slot| *slot < PHYSICS_BODY_SLOTS)
            .ok_or("Physics object group pose slot is malformed")?;
        (world.id, slot)
    };
    let world = def.nodes.iter().find(|node| node.id == world_id).unwrap();
    if world.type_id != "node.physics_world" {
        return Err("Physics object group world has the wrong type");
    }
    let top_body_wire = def
        .wires
        .iter()
        .find(|wire| {
            wire.to_node == world_id
                && wire.to_port == format!("body_{slot}")
                && wire.from_node == parts.producer_id
        })
        .ok_or("Physics object group body slot is missing")?;
    let _ = (body, top_body_wire);
    Ok(ImportedPhysicsBinding {
        world_id,
        body_slot: slot,
        body_id,
    })
}

fn remove_string_binding_target(def: &mut EffectGraphDef, node_id: &NodeId) {
    if let Some(meta) = def.preset_metadata.as_mut() {
        meta.string_bindings.retain(|binding| {
            !matches!(&binding.target, BindingTarget::Node { node_id: target, .. } if target == node_id)
        });
    }
}

/// Enable standard physics for one imported object.  Grouped imports expose a
/// `body` output and accept a `pose` input so the shared root world remains
/// outside the visual object group; the flattener then folds that boundary to
/// the same flat wiring used by a bare object.
#[derive(Debug)]
pub struct EnableSceneObjectPhysicsCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    object_index: u32,
    body_metadata: Vec<SceneParamMetadata>,
    world_metadata: Option<Vec<SceneParamMetadata>>,
    catalog_default: EffectGraphDef,
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    rejection: Option<String>,
}

impl EnableSceneObjectPhysicsCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        object_index: u32,
        body_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            object_index,
            body_metadata,
            world_metadata: None,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }

    pub fn with_world_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.world_metadata = Some(metadata);
        self
    }
}

impl Command for EnableSceneObjectPhysicsCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return;
        };
        let Ok(parts) = imported_object_parts(def, self.render_scene_node_id, self.object_index)
        else {
            self.rejection =
                Some("Enable Physics supports imported rigid glTF objects only".into());
            return;
        };
        if parts.render_indices.is_empty() {
            self.rejection = Some("Selected scene object has no render outputs".into());
            return;
        }
        if imported_physics_binding(def, &parts).is_ok() {
            self.rejection = Some("Selected object already has standard physics enabled".into());
            return;
        }
        let body_params = match imported_body_params(&parts.source, def) {
            Ok(mut params) => {
                if parts.compound_sources.len() > 1 {
                    match compound_materials_param(&parts.compound_sources) {
                        Ok(table) => {
                            params.insert("compound_materials".to_string(), table);
                        }
                        Err(reason) => {
                            self.rejection = Some(reason.into());
                            return;
                        }
                    }
                }
                params.insert(
                    "motion".to_string(),
                    SerializedParamValue::Enum { value: 1 },
                );
                params.insert(
                    "mass".to_string(),
                    SerializedParamValue::Float { value: 1.0 },
                );
                params.insert(
                    "friction".to_string(),
                    SerializedParamValue::Float { value: 0.5 },
                );
                params.insert(
                    "bounce".to_string(),
                    SerializedParamValue::Float { value: 0.15 },
                );
                params
            }
            Err(reason) => {
                self.rejection = Some(reason.into());
                return;
            }
        };
        let worlds: Vec<u32> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .map(|node| node.id)
            .collect();
        if worlds.len() > 1 {
            self.rejection = Some("Enable Physics requires one shared root Physics World".into());
            return;
        }
        let world_id = worlds
            .first()
            .copied()
            .unwrap_or_else(|| max_node_id_over(&def.nodes).saturating_add(1));
        let body_slot = first_free_physics_body_slot(&def.wires, world_id).unwrap_or({
            // A new world has no occupied slots; this branch is only used to
            // make the preflight expression total.
            0
        });
        if !worlds.is_empty() && first_free_physics_body_slot(&def.wires, world_id).is_none() {
            self.rejection = Some("Physics World has no free body slots".into());
            return;
        }
        let mut candidate = def.clone();
        let result = (|| {
            let def = &mut candidate;
            let previous = (
                def.nodes.clone(),
                def.wires.clone(),
                def.preset_metadata.clone(),
            );
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut taken);
            if worlds.is_empty() {
                let handle = dedup_handle("Physics World", &mut taken);
                def.nodes.push(fresh_scene_node(
                    next_id,
                    "node.physics_world",
                    Some(handle),
                    BTreeMap::new(),
                ));
                next_id += 1;
            }
            let body_id = next_id;
            next_id += 1;
            let body_handle = dedup_handle(&format!("{} Physics", parts.object_handle), &mut taken);
            let body_node_id = if let Some(group_id) = parts.group_id {
                let group = def
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == group_id)?
                    .group
                    .as_deref_mut()?;
                let input_id = next_id;
                next_id += 1;
                let output_id = next_id;
                let (node_id, _) = add_group_physics(
                    group,
                    body_id,
                    body_params.clone(),
                    body_handle,
                    input_id,
                    output_id,
                    parts.authored_transform_id,
                    parts.object_id,
                )
                .ok()?;
                def.wires.push(scene_build_wire(
                    group_id,
                    "body",
                    world_id,
                    &format!("body_{body_slot}"),
                ));
                def.wires.push(scene_build_wire(
                    world_id,
                    &format!("pose_{body_slot}"),
                    group_id,
                    "pose",
                ));
                node_id
            } else {
                let object_wire = def
                    .wires
                    .iter_mut()
                    .find(|wire| wire.to_node == parts.object_id && wire.to_port == "transform")?;
                object_wire.from_node = world_id;
                object_wire.from_port = format!("pose_{body_slot}");
                let body = fresh_scene_node(
                    body_id,
                    "node.rigid_body",
                    Some(body_handle),
                    body_params.clone(),
                );
                let body_node_id = body.node_id.clone();
                def.nodes.push(body);
                def.wires.push(scene_build_wire(
                    parts.authored_transform_id,
                    "transform",
                    body_id,
                    "transform",
                ));
                def.wires.push(scene_build_wire(
                    body_id,
                    "body",
                    world_id,
                    &format!("body_{body_slot}"),
                ));
                body_node_id
            };
            let string_binding = source_string_binding(def, &parts.source, body_node_id.clone());
            let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
                id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
                display_name: "Scene".to_string(),
                category: "Geometry".to_string(),
                osc_prefix: "scene".to_string(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: Vec::new(),
                bindings: Vec::new(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_modifier: None,
                scene_bounds: None,
            });
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                body_id,
                &body_node_id,
                "node.rigid_body",
                &format!("{} — Physics", parts.object_handle),
                &self.body_metadata,
                &body_params,
            );
            if worlds.is_empty()
                && let (Some(world), Some(world_metadata)) = (
                    def.nodes.iter().find(|node| node.id == world_id),
                    self.world_metadata.as_ref(),
                )
            {
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    world.id,
                    &world.node_id,
                    "node.physics_world",
                    "Physics World",
                    world_metadata,
                    &world.params,
                );
            }
            if let Some(binding) = string_binding {
                meta.string_bindings.push(binding);
            }
            Some(previous)
        })();
        if let Some(previous) = result {
            let _ =
                with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                    *def = candidate
                });
            self.prev = Some(previous);
            refresh_target_manifest(project, &self.target);
        } else {
            self.rejection =
                Some("Physics edit requires an unmodified imported object graph".into());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((nodes, wires, metadata)) = self.prev.take() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.nodes = nodes;
            def.wires = wires;
            def.preset_metadata = metadata;
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Enable Physics"
    }
    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

/// Remove the body/world wiring while leaving the imported visual object and
/// its authored transform intact.  The world node itself is retained as the
/// shared scene service, so disabling one object never invalidates another.
#[derive(Debug)]
pub struct DisableSceneObjectPhysicsCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    object_index: u32,
    catalog_default: EffectGraphDef,
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    rejection: Option<String>,
}

impl DisableSceneObjectPhysicsCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        object_index: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            object_index,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }
}

impl Command for DisableSceneObjectPhysicsCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }
    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return;
        };
        let Ok(parts) = imported_object_parts(def, self.render_scene_node_id, self.object_index)
        else {
            self.rejection =
                Some("Disable Physics supports imported rigid glTF objects only".into());
            return;
        };
        let Ok(binding) = imported_physics_binding(def, &parts) else {
            self.rejection = Some("Selected object does not have standard physics enabled".into());
            return;
        };
        let mut candidate = def.clone();
        let result = (|| {
            let def = &mut candidate;
            let previous = (
                def.nodes.clone(),
                def.wires.clone(),
                def.preset_metadata.clone(),
            );
            if let Some(group_id) = parts.group_id {
                let group = def
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == group_id)?
                    .group
                    .as_deref_mut()?;
                remove_group_physics(
                    group,
                    binding.body_id,
                    parts.object_id,
                    parts.authored_transform_id,
                )
                .ok()?;
                def.wires.retain(|wire| {
                    !(wire.from_node == group_id
                        && wire.to_node == binding.world_id
                        && wire.to_port == format!("body_{}", binding.body_slot)
                        || wire.from_node == binding.world_id
                            && wire.from_port == format!("pose_{}", binding.body_slot)
                            && wire.to_node == group_id)
                });
            } else {
                def.wires.retain(|wire| {
                    !((wire.from_node == binding.world_id
                        && wire.from_port == format!("pose_{}", binding.body_slot)
                        && wire.to_node == parts.object_id)
                        || (wire.from_node == binding.body_id
                            && wire.to_node == binding.world_id
                            && wire.to_port == format!("body_{}", binding.body_slot))
                        || (wire.from_node == parts.authored_transform_id
                            && wire.to_node == binding.body_id))
                });
                def.wires.push(scene_build_wire(
                    parts.authored_transform_id,
                    "transform",
                    parts.object_id,
                    "transform",
                ));
                def.nodes.retain(|node| node.id != binding.body_id);
            }
            let body_node_id = if parts.group_id.is_some() {
                // The body is inside the group; use its stable NodeId before
                // removing the node so the exposure sweep can prune it.
                previous
                    .0
                    .iter()
                    .flat_map(|node| node.group.as_ref().map(|g| g.nodes.iter()))
                    .flatten()
                    .find(|node| node.id == binding.body_id)
                    .map(|node| node.node_id.clone())
            } else {
                previous
                    .0
                    .iter()
                    .find(|node| node.id == binding.body_id)
                    .map(|node| node.node_id.clone())
            };
            if let Some(body_node_id) = body_node_id {
                prune_scene_object_metadata(def, std::slice::from_ref(&body_node_id));
                remove_string_binding_target(def, &body_node_id);
            }
            Some(previous)
        })();
        if let Some(previous) = result {
            let _ =
                with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                    *def = candidate
                });
            self.prev = Some(previous);
            refresh_target_manifest(project, &self.target);
        } else {
            self.rejection =
                Some("Physics edit requires an unmodified imported object graph".into());
        }
    }
    fn undo(&mut self, project: &mut Project) {
        let Some((nodes, wires, metadata)) = self.prev.take() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.nodes = nodes;
            def.wires = wires;
            def.preset_metadata = metadata;
        });
        refresh_target_manifest(project, &self.target);
    }
    fn description(&self) -> &str {
        "Disable Physics"
    }
    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

fn split_capacity_value(value: &SerializedParamValue) -> SerializedParamValue {
    let raw = match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        _ => return value.clone(),
    };
    let pieces = (raw.max(0.0).ceil() as u32).div_ceil(8).div_ceil(3) * 3;
    SerializedParamValue::Int {
        value: pieces.max(36) as i32,
    }
}

fn mutate_fragment_source(node: &mut EffectGraphNode, fragment_index: u32) -> Option<NodeId> {
    if node.type_id == "node.gltf_mesh_source" {
        node.params.insert(
            "fragment_count".to_string(),
            SerializedParamValue::Int { value: 8 },
        );
        node.params.insert(
            "fragment_index".to_string(),
            SerializedParamValue::Int {
                value: fragment_index as i32,
            },
        );
        if let Some(value) = node.params.get("max_capacity").cloned() {
            node.params
                .insert("max_capacity".to_string(), split_capacity_value(&value));
        }
        if let Some(value) = node.params.get("source_vertex_count").cloned() {
            node.params.insert(
                "source_vertex_count".to_string(),
                split_capacity_value(&value),
            );
        }
        return Some(node.node_id.clone());
    }
    node.group
        .as_deref_mut()?
        .nodes
        .iter_mut()
        .find_map(|child| mutate_fragment_source(child, fragment_index))
}

fn find_node_id_in_tree(nodes: &[EffectGraphNode], type_id: &str) -> Option<u32> {
    nodes.iter().find_map(|node| {
        (node.type_id == type_id).then_some(node.id).or_else(|| {
            node.group
                .as_deref()
                .and_then(|group| find_node_id_in_tree(&group.nodes, type_id))
        })
    })
}

/// Replace one imported object with eight independently rendered and
/// simulated fragments.  The command snapshots the complete authored level,
/// so rejection (unsupported source or fewer than eight world slots) is
/// atomic and undo/redo restores the exact original wiring and exposures.
#[derive(Debug)]
pub struct SplitSceneObjectCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    object_index: u32,
    body_metadata: Vec<SceneParamMetadata>,
    world_metadata: Option<Vec<SceneParamMetadata>>,
    catalog_default: EffectGraphDef,
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    rejection: Option<String>,
}

impl SplitSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        object_index: u32,
        body_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            object_index,
            body_metadata,
            world_metadata: None,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }

    pub fn with_world_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.world_metadata = Some(metadata);
        self
    }
}

impl Command for SplitSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return;
        };
        let Ok(parts) = imported_object_parts(def, self.render_scene_node_id, self.object_index)
        else {
            self.rejection = Some("Split Object supports imported rigid glTF objects only".into());
            return;
        };
        if parts.group_id.is_none() {
            self.rejection = Some(
                "Split supports imported object groups; group the object before splitting".into(),
            );
            return;
        }
        if parts.source.params.get("fragment_count").is_some_and(|value| match value {
            SerializedParamValue::Int { value } => *value > 1,
            SerializedParamValue::Float { value } => *value > 1.0,
            _ => false,
        }) {
            self.rejection = Some("This object is already a split piece".into());
            return;
        }
        let existing_binding = imported_physics_binding(def, &parts).ok();
        let existing_body = existing_binding.as_ref().and_then(|binding| {
            if let Some(group_id) = parts.group_id {
                def.nodes
                    .iter()
                    .find(|node| node.id == group_id)?
                    .group
                    .as_deref()?
                    .nodes
                    .iter()
                    .find(|node| node.id == binding.body_id)
            } else {
                def.nodes.iter().find(|node| node.id == binding.body_id)
            }
        });
        let body_float = |name: &str, fallback: f32| match existing_body
            .and_then(|node| node.params.get(name))
        {
            Some(SerializedParamValue::Float { value }) => *value,
            Some(SerializedParamValue::Int { value }) => *value as f32,
            _ => fallback,
        };
        let total_mass = body_float("mass", 1.0);
        let friction = body_float("friction", 0.5);
        let bounce = body_float("bounce", 0.15);
        let body_params = match imported_body_params(&parts.source, def) {
            Ok(mut params) => {
                params.insert(
                    "motion".to_string(),
                    SerializedParamValue::Enum { value: 1 },
                );
                params.insert(
                    "mass".to_string(),
                    SerializedParamValue::Float {
                        value: total_mass / 8.0,
                    },
                );
                params.insert(
                    "friction".to_string(),
                    SerializedParamValue::Float { value: friction },
                );
                params.insert(
                    "bounce".to_string(),
                    SerializedParamValue::Float { value: bounce },
                );
                params.insert(
                    "collider_parts".to_string(),
                    SerializedParamValue::Int { value: 1 },
                );
                params
            }
            Err(reason) => {
                self.rejection = Some(reason.into());
                return;
            }
        };
        let worlds: Vec<u32> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .map(|node| node.id)
            .collect();
        if worlds.len() > 1 {
            self.rejection = Some("Split Object requires one shared root Physics World".into());
            return;
        }
        let world_id = worlds
            .first()
            .copied()
            .unwrap_or_else(|| max_node_id_over(&def.nodes).saturating_add(1));
        let mut free_slots = Vec::new();
        let mut occupied = std::collections::HashSet::new();
        for wire in &def.wires {
            if wire.to_node == world_id
                && wire
                    .to_port
                    .strip_prefix("body_")
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_some_and(|slot| slot < PHYSICS_BODY_SLOTS)
            {
                occupied.insert(wire.to_port.clone());
            }
        }
        if let Some(binding) = existing_binding.as_ref() {
            free_slots.push(binding.body_slot);
        }
        for slot in 0..PHYSICS_BODY_SLOTS {
            if existing_binding
                .as_ref()
                .is_some_and(|binding| binding.body_slot == slot)
            {
                continue;
            }
            if !occupied.contains(&format!("body_{slot}")) {
                free_slots.push(slot);
            }
            if free_slots.len() == 8 {
                break;
            }
        }
        if free_slots.len() != 8 {
            self.rejection = Some("Physics World has fewer than eight free body slots".into());
            return;
        }
        let original_strings = def
            .preset_metadata
            .as_ref()
            .map(|meta| meta.string_bindings.clone())
            .unwrap_or_default();
        let mut candidate = def.clone();
        let result = (|| {
            let def = &mut candidate;
            let previous = (
                def.nodes.clone(),
                def.wires.clone(),
                def.preset_metadata.clone(),
            );
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut taken);
            if worlds.is_empty() {
                let handle = dedup_handle("Physics World", &mut taken);
                def.nodes.push(fresh_scene_node(
                    next_id,
                    "node.physics_world",
                    Some(handle),
                    BTreeMap::new(),
                ));
                next_id += 1;
            }
            let mut source_node = def
                .nodes
                .iter()
                .find(|node| node.id == parts.producer_id)?
                .clone();
            let mut removed = Vec::new();
            collect_node_ids(std::slice::from_ref(&source_node), &mut removed);
            if existing_binding.is_some() {
                strip_group_physics_for_split(source_node.group.as_deref_mut()?).ok()?;
            }
            let source_count = match def
                .nodes
                .iter()
                .find(|node| node.id == self.render_scene_node_id)?
                .params
                .get("objects")
            {
                Some(SerializedParamValue::Float { value }) => *value as u32,
                Some(SerializedParamValue::Int { value }) => (*value).max(0) as u32,
                _ => return None,
            };
            if self.object_index >= source_count {
                return None;
            }
            if let Some(binding) = existing_binding.as_ref() {
                def.wires.retain(|wire| {
                    !(wire.to_node == binding.world_id
                        && wire.to_port == format!("body_{}", binding.body_slot)
                        || wire.from_node == binding.world_id
                            && wire.from_port == format!("pose_{}", binding.body_slot)
                            && wire.to_node == parts.producer_id)
                        && !(wire.from_node == binding.body_id && wire.to_node == binding.world_id)
                        && !(wire.from_node == parts.authored_transform_id
                            && wire.to_node == binding.body_id)
                });
                if parts.group_id.is_none() {
                    def.nodes.retain(|node| node.id != binding.body_id);
                }
            }
            def.wires.retain(|wire| {
                wire.from_node != parts.producer_id
                    && !(wire.to_node == self.render_scene_node_id
                        && wire.to_port == format!("object_{}", self.object_index))
            });
            // The eight fragments occupy the replaced slot and the seven
            // additional slots immediately after it. Existing objects move
            // upward by seven slots.
            for wire in &mut def.wires {
                if wire.to_node == self.render_scene_node_id
                    && let Some(index) = wire
                        .to_port
                        .strip_prefix("object_")
                        .and_then(|s| s.parse::<u32>().ok())
                    && index > self.object_index
                {
                    wire.to_port = format!("object_{}", index + 7);
                }
            }
            let mut body_entries = Vec::new();
            for (fragment_index, body_slot) in free_slots.iter().copied().enumerate() {
                let mut map = Vec::new();
                let mut clone =
                    deep_clone_with_fresh_ids(&source_node, &mut next_id, &mut taken, &mut map);
                let piece_name = dedup_handle(&format!("{} Piece {}", parts.object_handle, fragment_index + 1), &mut taken);
                clone.handle = Some(piece_name.clone());
                if let Some(group) = clone.group.as_deref_mut() {
                    group.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")?.handle = Some(piece_name.clone());
                }
                let fragment_object_id =
                    find_node_id_in_tree(std::slice::from_ref(&clone), "node.scene_object")?;
                let fragment_transform_id =
                    find_node_id_in_tree(std::slice::from_ref(&clone), "node.transform_3d")?;
                mutate_fragment_source(&mut clone, fragment_index as u32)?;
                let mut cloned_source_params = clone_fragment_body_params(&body_params);
                cloned_source_params.insert(
                    "fragment_count".to_string(),
                    SerializedParamValue::Int { value: 8 },
                );
                cloned_source_params.insert(
                    "fragment_index".to_string(),
                    SerializedParamValue::Int {
                        value: fragment_index as i32,
                    },
                );
                cloned_source_params.insert(
                    "collider_parts".to_string(),
                    SerializedParamValue::Int { value: 1 },
                );
                let body_id = next_id;
                next_id += 1;
                let body_handle = dedup_handle(
                    &format!(
                        "{} Piece {} Physics",
                        parts.object_handle,
                        fragment_index + 1
                    ),
                    &mut taken,
                );
                let body_node_id = if let Some(group) = clone.group.as_deref_mut() {
                    let input_id = next_id;
                    next_id += 1;
                    let output_id = next_id;
                    next_id += 1;
                    let inner_transform = find_node_id_in_tree(&group.nodes, "node.transform_3d")?;
                    let inner_object = find_node_id_in_tree(&group.nodes, "node.scene_object")?;
                    let (body_node_id, _) = add_group_physics(
                        group,
                        body_id,
                        cloned_source_params.clone(),
                        body_handle,
                        input_id,
                        output_id,
                        inner_transform,
                        inner_object,
                    )
                    .ok()?;
                    body_node_id
                } else {
                    let body = fresh_scene_node(
                        body_id,
                        "node.rigid_body",
                        Some(body_handle),
                        cloned_source_params.clone(),
                    );
                    let body_node_id = body.node_id.clone();
                    clone_fragment_root_wires(
                        &mut def.wires,
                        fragment_object_id,
                        fragment_transform_id,
                        world_id,
                        body_slot,
                        body_id,
                    );
                    clone.group = None;
                    // The body is a root-level producer for a bare object.
                    def.nodes.push(body);
                    body_node_id
                };
                let clone_id = clone.id;
                def.nodes.push(clone);
                if parts.group_id.is_some() {
                    def.wires.push(scene_build_wire(
                        clone_id,
                        "body",
                        world_id,
                        &format!("body_{body_slot}"),
                    ));
                    def.wires.push(scene_build_wire(
                        world_id,
                        &format!("pose_{body_slot}"),
                        clone_id,
                        "pose",
                    ));
                }
                def.wires.push(scene_build_wire(
                    clone_id,
                    "object",
                    self.render_scene_node_id,
                    &format!("object_{}", self.object_index + fragment_index as u32),
                ));
                if let Some(meta) = def.preset_metadata.as_mut() {
                    for binding in &original_strings {
                        if let BindingTarget::Node { node_id, param } = &binding.target
                            && let Some((_, new_id)) = map.iter().find(|(old, _)| old == node_id)
                        {
                            let mut copied = binding.clone();
                            copied.target = BindingTarget::Node {
                                node_id: new_id.clone(),
                                param: param.clone(),
                            };
                            meta.string_bindings.push(copied);
                        }
                    }
                }
                if let Some(binding) =
                    original_strings
                        .iter()
                        .find_map(|binding| match &binding.target {
                            BindingTarget::Node { node_id, param }
                                if node_id == &parts.source.node_id && param == "path" =>
                            {
                                Some(StringBindingDef {
                                    id: binding.id.clone(),
                                    label: binding.label.clone(),
                                    default_value: binding.default_value.clone(),
                                    target: BindingTarget::Node {
                                        node_id: body_node_id.clone(),
                                        param: "path".to_string(),
                                    },
                                })
                            }
                            _ => None,
                        })
                    && let Some(meta) = def.preset_metadata.as_mut()
                {
                    meta.string_bindings.push(binding);
                }
                clone_sections::clone_scene_bindings(def, &map);
                // Separate sections keep every piece independently editable.
                if let Some(meta) = def.preset_metadata.as_mut() {
                    let ids: std::collections::HashSet<_> = meta.bindings.iter().filter_map(|binding| {
                        matches!(&binding.target, BindingTarget::Node { node_id, .. } if map.iter().any(|(_, new)| new == node_id)).then_some(binding.id.clone())
                    }).collect();
                    for param in &mut meta.params {
                        if ids.contains(&param.id) {
                            param.section = Some(format!("{} — {}", piece_name, param.section.as_deref().unwrap_or("Object")));
                        }
                    }
                }
                body_entries.push((body_id, body_node_id, cloned_source_params));
            }
            let render = def
                .nodes
                .iter_mut()
                .find(|node| node.id == self.render_scene_node_id)?;
            render.params.insert(
                "objects".to_string(),
                SerializedParamValue::Float {
                    value: (source_count + 7) as f32,
                },
            );
            def.nodes.retain(|node| node.id != parts.producer_id);
            prune_scene_object_metadata(def, &removed);
            for id in &removed {
                remove_string_binding_target(def, id);
            }
            if let Some(meta) = def.preset_metadata.as_mut() {
                for (index, (body_id, body_node_id, params)) in body_entries.iter().enumerate() {
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        *body_id,
                        body_node_id,
                        "node.rigid_body",
                        &format!("{} Piece {} — Physics", parts.object_handle, index + 1),
                        &self.body_metadata,
                        params,
                    );
                }
            }
            if worlds.is_empty()
                && let (Some(world), Some(world_metadata)) = (
                    def.nodes.iter().find(|node| node.id == world_id),
                    self.world_metadata.as_ref(),
                )
                && let Some(meta) = def.preset_metadata.as_mut()
            {
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    world.id,
                    &world.node_id,
                    "node.physics_world",
                    "Physics World",
                    world_metadata,
                    &world.params,
                );
            }
            Some(previous)
        })();
        if let Some(previous) = result {
            let _ =
                with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                    *def = candidate
                });
            self.prev = Some(previous);
            refresh_target_manifest(project, &self.target);
        } else {
            self.rejection =
                Some("Physics edit requires an unmodified imported object graph".into());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((nodes, wires, metadata)) = self.prev.take() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.nodes = nodes;
            def.wires = wires;
            def.preset_metadata = metadata;
        });
        refresh_target_manifest(project, &self.target);
    }
    fn description(&self) -> &str {
        "Split Object into 8 Pieces"
    }
    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

fn clone_fragment_body_params(
    body_params: &BTreeMap<String, SerializedParamValue>,
) -> BTreeMap<String, SerializedParamValue> {
    body_params.clone()
}

fn clone_fragment_root_wires(
    wires: &mut Vec<EffectGraphWire>,
    object_id: u32,
    transform_id: u32,
    world_id: u32,
    body_slot: u32,
    body_id: u32,
) {
    wires.push(scene_build_wire(
        world_id,
        &format!("pose_{body_slot}"),
        object_id,
        "transform",
    ));
    wires.push(scene_build_wire(
        transform_id,
        "transform",
        body_id,
        "transform",
    ));
    wires.push(scene_build_wire(
        body_id,
        "body",
        world_id,
        &format!("body_{body_slot}"),
    ));
}

fn remap_physics_wire(
    wire: &EffectGraphWire,
    node_map: &std::collections::HashMap<u32, u32>,
    world_id: u32,
    old_slot: u32,
    new_slot: u32,
    render_id: u32,
    old_object_indices: &[u32],
    new_object_index: u32,
) -> EffectGraphWire {
    let map_node = |id: u32| node_map.get(&id).copied().unwrap_or(id);
    let mut from_port = wire.from_port.clone();
    let mut to_port = wire.to_port.clone();
    if wire.from_node == world_id && wire.from_port == format!("pose_{old_slot}") {
        from_port = format!("pose_{new_slot}");
    }
    if wire.to_node == world_id && wire.to_port == format!("body_{old_slot}") {
        to_port = format!("body_{new_slot}");
    }
    if wire.to_node == render_id
        && let Some(old_index) = wire
            .to_port
            .strip_prefix("object_")
            .and_then(|value| value.parse::<u32>().ok())
        && let Some(part) = old_object_indices.iter().position(|index| *index == old_index)
    {
        to_port = format!("object_{}", new_object_index + part as u32);
    }
    EffectGraphWire {
        from_node: map_node(wire.from_node),
        from_port,
        to_node: map_node(wire.to_node),
        to_port,
    }
}

fn append_physics_duplicate(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    physics: &PhysicsSceneObject,
    render_id: u32,
    source_indices: &[u32],
    new_index: u32,
    new_slot: u32,
    node_id_map: &mut Vec<(NodeId, NodeId)>,
) -> Option<()> {
    let mut next_id = max_node_id_over(nodes) + 1;
    let mut taken = std::collections::HashSet::new();
    collect_all_handles(nodes, &mut taken);
    let owned = physics
        .owned_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut clones = std::collections::HashMap::<u32, EffectGraphNode>::new();
    for old_id in &physics.owned_ids {
        let source = nodes.iter().find(|node| node.id == *old_id)?;
        let clone = deep_clone_with_fresh_ids(source, &mut next_id, &mut taken, node_id_map);
        clones.insert(*old_id, clone);
    }
    let source_object = nodes.iter().find(|node| node.id == physics.object_id)?;
    let cloned_handle = source_object.handle.as_ref().map(|handle| {
        let mut suffix = 2;
        loop {
            let candidate = format!("{handle} {suffix}");
            if !taken.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        }
    });
    if let Some(clone) = clones.get_mut(&physics.object_id) {
        clone.handle = cloned_handle;
        clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
    }
    if let Some(clone) = clones.get_mut(&physics.transform_id) {
        let current = match clone.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        };
        clone.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float {
                value: current + 0.5,
            },
        );
    }
    if physics.grouped
        && let Some(clone) = clones.get_mut(&physics.object_id)
        && let Some(transform) = clone.group.as_deref_mut().and_then(|group| {
            group
                .nodes
                .iter_mut()
                .find(|node| node.type_id == "node.transform_3d")
        })
    {
        let current = match transform.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        };
        transform.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float {
                value: current + 0.5,
            },
        );
    }

    let mut node_map = std::collections::HashMap::new();
    for (old_id, clone) in &clones {
        node_map.insert(*old_id, clone.id);
    }
    for old_id in &physics.owned_ids {
        nodes.push(clones.remove(old_id)?);
    }
    for wire in wires.clone() {
        if owned.contains(&wire.from_node) || owned.contains(&wire.to_node) {
            wires.push(remap_physics_wire(
                &wire,
                &node_map,
                physics.world_id,
                physics.body_slot,
                new_slot,
                render_id,
                source_indices,
                new_index,
            ));
        }
    }
    Some(())
}

fn graph_level<'a>(
    def: &'a EffectGraphDef,
    scope: &[u32],
) -> Option<(&'a [EffectGraphNode], &'a [EffectGraphWire])> {
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    for group_id in scope {
        let group = nodes
            .iter()
            .find(|node| node.id == *group_id)?
            .group
            .as_deref()?;
        nodes = group.nodes.as_slice();
        wires = group.wires.as_slice();
    }
    Some((nodes, wires))
}

fn target_string_bindings(
    project: &Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
) -> Option<Option<Vec<StringBindingDef>>> {
    let def = project.graph_for_target(target, Some(catalog_default))?;
    Some(
        def.preset_metadata
            .as_ref()
            .map(|meta| meta.string_bindings.clone()),
    )
}

/// Remove exposure bindings whose stable target lives in a deleted object
/// subtree. Shared binding ids are retained when another target still uses
/// them (the importer deliberately fans out one outer control to many nodes).
/// Return only ids that no longer have any surviving binding so the host
/// manifest and its modulation collections can be pruned by the caller.
pub(super) fn prune_scene_object_metadata(def: &mut EffectGraphDef, removed: &[NodeId]) -> Vec<String> {
    let Some(meta) = def.preset_metadata.as_mut() else {
        return Vec::new();
    };
    let removed: std::collections::HashSet<&NodeId> = removed.iter().collect();
    let removed_numeric: Vec<String> = meta
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, .. } if removed.contains(node_id) => {
                Some(binding.id.clone())
            }
            _ => None,
        })
        .collect();
    let removed_strings: Vec<String> = meta
        .string_bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, .. } if removed.contains(node_id) => {
                Some(binding.id.clone())
            }
            _ => None,
        })
        .collect();
    meta.bindings.retain(|binding| {
        !matches!(&binding.target, BindingTarget::Node { node_id, .. } if removed.contains(node_id))
    });
    meta.string_bindings.retain(|binding| {
        !matches!(&binding.target, BindingTarget::Node { node_id, .. } if removed.contains(node_id))
    });

    let surviving_numeric: std::collections::BTreeSet<&str> = meta
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect();
    let surviving_strings: std::collections::BTreeSet<&str> = meta
        .string_bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect();
    let numeric_to_prune: std::collections::BTreeSet<&str> = removed_numeric
        .iter()
        .map(String::as_str)
        .filter(|id| !surviving_numeric.contains(id))
        .collect();
    let strings_to_prune: std::collections::BTreeSet<&str> = removed_strings
        .iter()
        .map(String::as_str)
        .filter(|id| !surviving_strings.contains(id))
        .collect();
    meta.params
        .retain(|param| !numeric_to_prune.contains(param.id.as_str()));
    meta.string_params
        .retain(|param| !strings_to_prune.contains(param.id.as_str()));

    numeric_to_prune
        .into_iter()
        .chain(strings_to_prune)
        .map(str::to_string)
        .collect()
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
    prev_string_bindings: Option<Option<Vec<StringBindingDef>>>,
    /// Whole metadata snapshot, including numeric scene exposures cloned for
    /// root-level physics objects.
    prev_metadata: Option<Option<PresetMetadata>>,
    /// Cached successful result. Redo restores these exact ids after checking
    /// that the graph and string bindings still match the pre-edit baseline.
    after: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
    after_string_bindings: Option<Option<Vec<StringBindingDef>>>,
    after_metadata: Option<Option<PresetMetadata>>,
    /// Live manifest/modulation state before and after the duplicate. A
    /// structural refresh intentionally rebuilds the manifest, so retaining
    /// these snapshots keeps authored values stable across undo/redo too.
    prev_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    rejection: Option<String>,
    applied: bool,
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
            prev_metadata: None,
            after: None,
            after_string_bindings: None,
            after_metadata: None,
            prev_instance: None,
            after_instance: None,
            rejection: None,
            applied: false,
        }
    }
}

impl Command for DuplicateSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.applied = false;
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let src_k = self.source_index;

        if let (Some(after), Some(_after_strings)) =
            (self.after.as_ref(), self.after_string_bindings.as_ref())
        {
            let current_level = project
                .graph_for_target(&self.target, Some(&self.catalog_default))
                .and_then(|def| graph_level(def, &scope))
                .map(|(nodes, wires)| (nodes.to_vec(), wires.to_vec()));
            let baseline_level = self
                .prev
                .as_ref()
                .map(|(nodes, wires)| (nodes.clone(), wires.clone()));
            let current_strings =
                target_string_bindings(project, &self.target, &self.catalog_default);
            let current_metadata = project
                .graph_for_target(&self.target, Some(&self.catalog_default))
                .map(|def| def.preset_metadata.clone());
            if current_level != baseline_level
                || current_strings != self.prev_string_bindings
                || current_metadata != self.prev_metadata
            {
                self.rejection = Some(
                    "Duplicate Object redo rejected: graph or source bindings changed since undo"
                        .into(),
                );
                return;
            }
            let restored =
                with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    *nodes = after.0.clone();
                    *wires = after.1.clone();
                    Some(())
                })
                .flatten()
                .is_some();
            if !restored {
                self.rejection = Some("Duplicate Object redo target is unavailable".into());
                return;
            }
            let _ = with_target_graph_def_mut(project, &self.target, |def| {
                def.preset_metadata = self.after_metadata.clone().flatten();
            });
            // Keep freshly cloned scene exposures visible to the live panel
            // after redo; otherwise a save/load is required before the new
            // binding slots can be edited.
            refresh_target_manifest(project, &self.target);
            if let (Some(snapshot), Some(instance)) = (
                self.after_instance.clone(),
                resolve_target_instance(&self.target, project),
            ) {
                snapshot.restore(instance);
            }
            self.applied = true;
            return;
        }

        let physics_match = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .and_then(|def| {
                let (nodes, wires) = graph_level(def, &scope)?;
                let source_id = object_producer_id(wires, render_id, src_k)?;
                Some(physics_scene_object_match(
                    nodes, wires, render_id, src_k, source_id,
                ))
            });
        if let Some(PhysicsSceneObjectMatch::Malformed(reason)) = &physics_match {
            self.rejection = Some((*reason).into());
            return;
        }
        if let Some(PhysicsSceneObjectMatch::Valid(physics)) = physics_match.as_ref() {
            if !scope.is_empty() {
                self.rejection =
                    Some("Duplicate Object physics ownership requires a root-level scene".into());
                return;
            }
            if physics.copies {
                self.rejection =
                    Some("Duplicate Object cannot duplicate a Physics World copies object".into());
                return;
            }
            if project
                .graph_for_target(&self.target, Some(&self.catalog_default))
                .and_then(|def| graph_level(def, &scope))
                .and_then(|(_, wires)| first_free_physics_body_slot(wires, physics.world_id))
                .is_none()
            {
                self.rejection =
                    Some("Duplicate Object physics world has no free body slot".into());
                return;
            }
        }

        let baseline_instance = resolve_target_instance(&self.target, project)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        let baseline_strings = target_string_bindings(project, &self.target, &self.catalog_default);
        let baseline_metadata = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .map(|def| def.preset_metadata.clone());
        let mut node_id_map: Vec<(NodeId, NodeId)> = Vec::new();
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                // Document ids and handles are global even when the edit is
                // addressed through a nested scope. Seed both allocators
                // from the full tree before borrowing the target level.
                let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
                let mut taken = std::collections::HashSet::new();
                collect_all_handles(&def.nodes, &mut taken);
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                let current_objects = match nodes
                    .iter()
                    .find(|n| n.id == render_id)?
                    .params
                    .get("objects")
                {
                    Some(SerializedParamValue::Float { value }) => *value,
                    Some(SerializedParamValue::Int { value }) => *value as f32,
                    _ => 0.0,
                };
                let new_k = current_objects as u32;
                if let Some(PhysicsSceneObjectMatch::Valid(physics)) = physics_match.as_ref() {
                    let new_slot = first_free_physics_body_slot(wires, physics.world_id)?;
                    append_physics_duplicate(
                        nodes,
                        wires,
                        physics,
                        render_id,
                        &physics.render_indices,
                        new_k,
                        new_slot,
                        &mut node_id_map,
                    )?;
                    let part_count = physics.render_indices.len() as f32;
                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: current_objects + part_count,
                        },
                    );
                } else {
                    let source_id = object_producer_id(wires, render_id, src_k)?;
                    let source_node = nodes.iter().find(|n| n.id == source_id)?.clone();
                    let mut source_outputs: Vec<(u32, String)> = wires
                        .iter()
                        .filter_map(|wire| {
                            if wire.from_node != source_id
                                || wire.to_node != render_id
                            {
                                return None;
                            }
                            wire.to_port
                                .strip_prefix("object_")
                                .and_then(|value| value.parse::<u32>().ok())
                                .map(|index| (index, wire.from_port.clone()))
                        })
                        .collect();
                    source_outputs.sort_unstable();
                    source_outputs.dedup_by_key(|(index, _)| *index);
                    if source_outputs.is_empty() {
                        return None;
                    }

                    let mut clone = deep_clone_with_fresh_ids(
                        &source_node,
                        &mut next_id,
                        &mut taken,
                        &mut node_id_map,
                    );
                    // D11's exact top-level convention (handle + " 2") overrides
                    // whatever `deep_clone_with_fresh_ids`'s generic dedup pass
                    // assigned to the TOP node.
                    let cloned_handle = source_node.handle.as_ref().map(|h| format!("{h} 2"));
                    clone.handle = cloned_handle.clone();
                    clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));

                    // D6: keep a grouped scene_object's name in sync with its
                    // enclosing group, as Add/importer and Rename do.
                    if let Some(body) = clone.group.as_deref_mut() {
                        if let Some(inner_object) = body
                            .nodes
                            .iter_mut()
                            .find(|n| n.type_id == "node.scene_object")
                        {
                            inner_object.handle = cloned_handle;
                        }
                        if let Some(transform_node) = body
                            .nodes
                            .iter_mut()
                            .find(|n| n.type_id == "node.transform_3d")
                        {
                            let cur = match transform_node.params.get("pos_x") {
                                Some(SerializedParamValue::Float { value }) => *value,
                                _ => 0.0,
                            };
                            transform_node.params.insert(
                                "pos_x".to_string(),
                                SerializedParamValue::Float { value: cur + 0.5 },
                            );
                        }
                    }
                    let clone_id = clone.id;
                    nodes.push(clone);
                    for (part, _) in source_outputs.iter().enumerate() {
                        wires.push(scene_build_wire(
                            clone_id,
                            &source_outputs[part].1,
                            render_id,
                            &format!("object_{}", new_k + part as u32),
                        ));
                    }
                    let part_count = source_outputs.len() as f32;
                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: current_objects + part_count,
                        },
                    );
                }

                Some(prev)
            });
        self.prev = result.flatten();
        if self.prev.is_none() {
            // The clone itself was refused (unresolvable source/level) — no
            // subtree was cloned, so there's nothing to sweep bindings for.
            self.rejection = Some("Duplicate Object source object is unavailable".into());
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
        if !node_id_map.is_empty() {
            let _ = with_target_graph_def_mut(project, &self.target, |def| {
                let meta = def.preset_metadata.as_mut()?;
                let new_entries: Vec<StringBindingDef> = meta
                    .string_bindings
                    .iter()
                    .filter_map(|b| match &b.target {
                        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                            node_id_map
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
                                })
                        }
                        manifold_core::effect_graph_def::BindingTarget::Composite { .. } => None,
                        manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                            ..
                        } => None,
                    })
                    .collect();
                meta.string_bindings.extend(new_entries);
                Some(())
            });
        }
        if !node_id_map.is_empty() {
            let _ = with_target_graph_def_mut(project, &self.target, |def| {
                clone_sections::clone_scene_bindings(def, &node_id_map);
            });
        }
        self.prev_string_bindings = baseline_strings;
        self.prev_metadata = baseline_metadata;
        self.after = with_target_graph_def_mut(project, &self.target, |def| {
            graph_level(def, &scope).map(|(nodes, wires)| (nodes.to_vec(), wires.to_vec()))
        })
        .flatten();
        self.after_string_bindings =
            target_string_bindings(project, &self.target, &self.catalog_default);
        self.after_metadata = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .map(|def| def.preset_metadata.clone());
        self.applied = self.after.is_some();
        if self.applied {
            // Physics duplicates clone their numeric exposure definitions.
            // Rebuild the host manifest now so the new rows are immediately
            // editable and survive execute/undo/redo without save/load.
            refresh_target_manifest(project, &self.target);
            self.prev_instance = baseline_instance;
            self.after_instance = resolve_target_instance(&self.target, project)
                .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(prev_metadata) = self.prev_metadata.clone() {
            let _ = with_target_graph_def_mut(project, &self.target, |def| {
                def.preset_metadata = prev_metadata;
            });
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
        refresh_target_manifest(project, &self.target);
        if let (Some(snapshot), Some(instance)) = (
            self.prev_instance.clone(),
            resolve_target_instance(&self.target, project),
        ) {
            snapshot.restore(instance);
        }
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Duplicate Object"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.applied
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
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
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
                    params.insert(
                        "intensity".to_string(),
                        SerializedParamValue::Float { value: 1.0 },
                    );
                    params.insert(
                        "fill".to_string(),
                        SerializedParamValue::Float { value: 0.0 },
                    );

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
                    layer_types: None,
                    params: Vec::new(),
                    bindings: Vec::new(),
                    param_aliases: Vec::new(),
                    value_aliases: Vec::new(),
                    string_params: Vec::new(),
                    string_bindings: Vec::new(),
                    scene_modifier: None,
                    scene_bounds: None,
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
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let pos = self.pos;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
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

                    let mut fog_node = scene_build_node(
                        fog_id,
                        "node.atmosphere",
                        Some("fog".to_string()),
                        params,
                    );
                    fog_node.editor_pos = Some(pos);
                    let fog_node_id = fog_node.node_id.clone();
                    nodes.push(fog_node);
                    wires.push(scene_build_wire(
                        fog_id,
                        "atmosphere",
                        render_id,
                        "atmosphere",
                    ));

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
                    layer_types: None,
                    params: Vec::new(),
                    bindings: Vec::new(),
                    param_aliases: Vec::new(),
                    value_aliases: Vec::new(),
                    string_params: Vec::new(),
                    string_bindings: Vec::new(),
                    scene_modifier: None,
                    scene_bounds: None,
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

                // BUG-p6x7: fog density is per-world-unit — override the generic
                // 0..1 band to (0, 2/radius) when scene_bounds are present so the
                // slider covers the useful range for this scene's scale.
                if let Some((new_min, new_max)) = fog_density_range(meta.scene_bounds)
                    && let Some(density_spec) = meta.params.iter_mut().find(|p| {
                        meta.bindings.iter().any(|b| {
                            b.id == p.id
                                && matches!(
                                    &b.target,
                                    BindingTarget::Node { node_id, param }
                                        if *node_id == fog_node_id && param == "fog_density"
                                )
                        })
                    })
                {
                    density_spec.min = new_min.min(density_spec.default_value);
                    density_spec.max = new_max.max(density_spec.default_value);
                }

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

/// BUG-p6x7: fog `density` is per-world-unit (`1 - exp(-density·distance)`),
/// so the shared scene-scaled range table handles the radius derivation.
/// Delegates to `manifold_core::scene_exposure::scene_scaled_range`.
pub(crate) fn fog_density_range(scene_bounds: Option<([f32; 3], [f32; 3])>) -> Option<(f32, f32)> {
    let bounds = scene_bounds?;
    let radius = manifold_core::scene_exposure::scene_radius_from_bounds(bounds);
    manifold_core::scene_exposure::scene_scaled_range("node.atmosphere", "fog_density", radius)
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let object_id = self.scene_object_node_id;
        let pos = self.pos;
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                let xf_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                let params = BTreeMap::new();
                let mut xf_node = scene_build_node(
                    xf_id,
                    "node.transform_3d",
                    Some("transform".to_string()),
                    params,
                );
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
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
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
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let new_nodes = self.new_nodes.clone();
        let new_wires = self.new_wires.clone();
        let objects = self.new_objects_count;
        let new_card_params = self.new_card_params.clone();
        let new_card_bindings = self.new_card_bindings.clone();
        let new_string_bindings = self.new_string_bindings.clone();
        let result =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
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
                            id: manifold_core::PresetTypeId::from_string(
                                "UnnamedScene".to_string(),
                            ),
                            display_name: "Scene".to_string(),
                            category: "Geometry".to_string(),
                            osc_prefix: "scene".to_string(),
                            legacy_discriminant: None,
                            available: true,
                            is_line_based: false,
                            layer_types: None,
                            params: Vec::new(),
                            bindings: Vec::new(),
                            param_aliases: Vec::new(),
                            value_aliases: Vec::new(),
                            string_params: Vec::new(),
                            string_bindings: Vec::new(),
                            scene_modifier: None,
                            scene_bounds: None,
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
                    SerializedParamValue::Float {
                        value: objects as f32,
                    },
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
type RenameSceneObjectPrev = (Option<u32>, Option<String>, Option<(u32, Option<String>)>);

fn find_scene_object_scope(
    nodes: &[EffectGraphNode],
    target_id: u32,
    scope: &mut Vec<u32>,
) -> Option<Vec<u32>> {
    for node in nodes {
        if node.id == target_id && node.type_id == "node.scene_object" {
            return Some(scope.clone());
        }
        if let Some(group) = node.group.as_deref() {
            scope.push(node.id);
            if let Some(found) = find_scene_object_scope(&group.nodes, target_id, scope) {
                return Some(found);
            }
            scope.pop();
        }
    }
    None
}

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
    /// The containing group path when the panel addressed a child directly by
    /// its scene_object id.  In that mode the enclosing group keeps its own
    /// handle; only the child handle and its section metadata change.
    nested_scope: Option<Vec<u32>>,
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
        Self {
            target,
            scope_path,
            object_node_id,
            new_handle,
            catalog_default,
            prev: None,
            nested_scope: None,
            swept: Vec::new(),
        }
    }
}

impl Command for RenameSceneObjectCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let producer_id = self.object_node_id;
        let new_handle = self.new_handle.clone();
        let first_time = self.prev.is_none();

        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                if new_handle.is_empty() || new_handle.contains('/') {
                    return None;
                }
                // Reject a collision with any sibling's handle at this level
                // (matching RenameGroupCommand's own guard).
                if nodes.iter().any(|n| {
                    n.id != producer_id && n.handle.as_deref() == Some(new_handle.as_str())
                }) {
                    return None;
                }
                let Some(producer_index) = nodes.iter().position(|n| n.id == producer_id) else {
                    if !scope.is_empty() {
                        return None;
                    }
                    let mut nested = Vec::new();
                    let nested_scope = find_scene_object_scope(&def.nodes, producer_id, &mut nested)?;
                    if nested_scope.is_empty() {
                        return None;
                    }
                    let (parent_nodes, _parent_wires) = descend_level(&mut def.nodes, &mut def.wires, &nested_scope)?;
                    if parent_nodes.iter().any(|node| {
                        node.id != producer_id && node.handle.as_deref() == Some(new_handle.as_str())
                    }) {
                        return None;
                    }
                    let child = parent_nodes.iter_mut().find(|node| node.id == producer_id)?;
                    let previous = child.handle.clone();
                    child.handle = Some(new_handle.clone());
                    let mut inside = Vec::new();
                    collect_node_ids(std::slice::from_ref(child), &mut inside);
                    return Some(((Some(producer_id), previous, None, inside), Some(nested_scope)));
                };
                let producer = &mut nodes[producer_index];

                if producer.type_id == GROUP_TYPE_ID {
                    // Grouped shape (Add / importer / merge): rename the group
                    // AND the inner scene_object's own handle stays in sync
                    // (D6's single-writer-of-both posture).
                    let prev_group_handle = producer.handle.clone();
                    producer.handle = Some(new_handle.clone());
                    let body = producer.group.as_deref_mut()?;
                    let scene_object_ids: Vec<u32> = body
                        .nodes
                        .iter()
                        .filter(|node| node.type_id == "node.scene_object")
                        .map(|node| node.id)
                        .collect();
                    let (scene_object_id, prev_object_handle) = if scene_object_ids.len() == 1
                        && !body.wires.iter().any(|wire| wire.to_port == "parent_transform") {
                        let scene_object = body
                            .nodes
                            .iter_mut()
                            .find(|node| node.id == scene_object_ids[0])?;
                        let previous = scene_object.handle.clone();
                        scene_object.handle = Some(new_handle.clone());
                        (Some(scene_object.id), previous)
                    } else {
                        // A compound parent owns several independently named
                        // scene objects. Renaming the parent must not rename
                        // the first child as a side effect.
                        (None, None)
                    };

                    let mut inside = Vec::new();
                    collect_node_ids(&body.nodes, &mut inside);
                    Some(((
                        scene_object_id,
                        prev_object_handle,
                        Some((producer_id, prev_group_handle)),
                        inside,
                    ), None))
                } else {
                    // Ungrouped bare scene_object: just its own handle, no group
                    // to keep in sync, no card-section sweep possible.
                    let prev_object_handle = producer.handle.clone();
                    producer.handle = Some(new_handle.clone());
                    Some(((Some(producer_id), prev_object_handle, None, Vec::new()), None))
                }
            });
        let Some(((scene_object_id, prev_object_handle, prev_group, inside), nested_scope)) = captured.flatten()
        else {
            return;
        };
        if first_time {
            self.prev = Some((scene_object_id, prev_object_handle, prev_group.clone()));
            self.nested_scope = nested_scope;
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
            if matches!(self.target, GraphTarget::SceneModifier { .. }) {
                self.swept = super::param_sections::rename_modifier_sections(
                    project,
                    &self.target,
                    &inside,
                    &old_name,
                    &self.new_handle,
                );
            }
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
                        manifold_core::effect_graph_def::BindingTarget::Node {
                            node_id, ..
                        } => inside.contains(node_id),
                        manifold_core::effect_graph_def::BindingTarget::Composite { .. } => false,
                        manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                            ..
                        } => false,
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

        if matches!(self.target, GraphTarget::SceneModifier { .. }) {
            for (id, section) in self.swept.drain(..) {
                super::param_sections::set_modifier_section(project, &self.target, &id, section);
            }
        }

        let Some((scene_object_id, prev_object_handle, prev_group)) = self.prev.clone() else {
            return;
        };
        let scope = self.nested_scope.clone().unwrap_or_else(|| self.scope_path.clone());
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
            else {
                return;
            };
            if let Some((group_id, prev_group_handle)) = prev_group {
                if let Some(group) = nodes.iter_mut().find(|n| n.id == group_id) {
                    group.handle = prev_group_handle;
                    if let Some(scene_object_id) = scene_object_id
                        && let Some(body) = group.group.as_deref_mut()
                        && let Some(scene_object) =
                            body.nodes.iter_mut().find(|n| n.id == scene_object_id)
                    {
                        scene_object.handle = prev_object_handle;
                    }
                }
            } else if let Some(scene_object_id) = scene_object_id
                && let Some(node) = nodes.iter_mut().find(|n| n.id == scene_object_id)
            {
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
        Self {
            target,
            scope_path,
            node_doc_id,
            new_handle,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for SetNodeHandleCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.node_doc_id;
        let new_handle = self.new_handle.clone();
        let first_time = self.prev.is_none();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
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
mod tests;
