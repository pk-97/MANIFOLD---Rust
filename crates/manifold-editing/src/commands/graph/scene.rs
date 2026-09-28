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
mod clone_values;
mod fluid;
mod physics;
mod physics_match;
mod split;

use physics::*;
use physics_match::*;
use manifold_core::scene_object_migration::loose_scene_object_owned_ids;
use split::*;
pub use physics::{
    scene_object_physics_eligibility, DisableSceneObjectPhysicsCommand,
    EnableSceneObjectPhysicsCommand,
};
pub use split::SplitSceneObjectCommand;

#[cfg(test)]
mod fluid_object_lifecycle_tests;

pub use fluid::*;

/// Scene commands snapshot the complete owner, including modifier-local edits.
fn restore_scene_owner_graph(
    project: &mut Project,
    target: &GraphTarget,
    graph: Option<EffectGraphDef>,
) {
    if let Some(owner) = target.host_target() {
        install_target_graph(project, owner, graph);
    }
}

fn scene_object_source_matches(
    def: Option<&EffectGraphDef>, scope: &[u32], render: u32, index: u32, expected: Option<&NodeId>,
) -> bool {
    expected.is_none_or(|expected| def.and_then(|def| {
        let (nodes, wires) = graph_level(def, scope)?;
        let producer = object_producer_id(wires, render, index)?;
        nodes.iter().find(|node| node.id == producer).map(|node| &node.node_id)
    }) == Some(expected))
}

/// Modifier-local ids are mapped to owner ids by `refresh_target_manifest`.
fn prune_scene_target_params(project: &mut Project, target: &GraphTarget, removed: &[String]) {
    if !matches!(target, GraphTarget::SceneModifier { .. })
        && let Some(instance) = project.graph_target_owner_mut(target)
    {
        prune_instance_params(instance, removed);
    }
}

use super::{
    InstanceLayerSnapshot, collect_node_ids, dedup_handle, descend_level, install_target_graph,
    prune_instance_params, refresh_target_manifest, resolve_target_instance, scene_build_node,
    scene_build_wire, with_existing_target_graph_mut, with_target_graph_mut,
};

/// The add-object gesture (D7): one undoable composite edit that (1) bumps
/// `render_scene`'s `objects` count by one, (2) builds a new group named
/// "Object N" containing a placeholder `node.cube_mesh` + a tinted
/// `node.pbr_material` + a `node.transform_3d`, wired to a
/// `system.group_output` boundary exposing `vertices`/`material`/`transform`,
/// (3) wires the group's three outputs to the new `mesh_k`/`material_k`/
/// `transform_k` ports on `render_scene`. Mirrors `GroupNodesCommand`'s
/// whole-level snapshot/restore shape — this is a structural composite edit
/// exactly like a group-creation, so undo restores the pre-edit `(nodes,
/// wires)` verbatim rather than reversing each sub-step by hand.
///
/// `next_index` remains in the constructor for the scene-setup action ABI, but
/// is only a stale UI hint. The content-owned `render_scene.objects` count is
/// validated and resolved at execution time so logical parent rows cannot
/// overwrite a compound object's later physical parts.
#[derive(Debug)]
pub struct AddSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
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
    // Reuse the accepted graph identities when redo restores this insertion.
    after: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
    rejection: Option<&'static str>,
}

impl AddSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        _next_index: u32,
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
            centroid,
            material_metadata,
            transform_metadata,
            scene_object_metadata,
            physics_body_metadata: None,
            physics_material_metadata: None,
            catalog_default,
            prev: None,
            after: None,
            rejection: None,
        }
    }

    fn capture_after(&mut self, project: &Project) {
        if self.prev.is_none() { return; }
        self.after = project.graph_for_target(&self.target, Some(&self.catalog_default))
            .and_then(|def| graph_level(def, &self.scope_path)
                .map(|(nodes, wires)| (nodes.to_vec(), wires.to_vec(), def.preset_metadata.clone())));
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

    fn execute_physics(
        &mut self,
        project: &mut Project,
        world_id: u32,
        body_slot: u32,
        k: u32,
    ) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
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
        self.capture_after(project);
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

impl Command for AddSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        if let Some((nodes, wires, metadata)) = self.after.as_ref() {
            self.rejection = None;
            let unchanged = project.graph_for_target(&self.target, Some(&self.catalog_default))
                .and_then(|def| graph_level(def, &self.scope_path).map(|(nodes, wires)| (def, nodes, wires)))
                .zip(self.prev.as_ref())
                .is_some_and(|((def, nodes, wires), (before_nodes, before_wires, before_metadata))|
                    nodes == before_nodes && wires == before_wires && &def.preset_metadata == before_metadata);
            if !unchanged {
                self.rejection = Some("Add Object redo rejected: graph changed since undo");
                return;
            }
            let restored = with_existing_target_graph_mut(project, &self.target, true, |def| {
                let (target_nodes, target_wires) = descend_level(&mut def.nodes, &mut def.wires, &self.scope_path)?;
                target_nodes.clone_from(nodes);
                target_wires.clone_from(wires);
                def.preset_metadata.clone_from(metadata);
                Some(())
            }).flatten().is_some();
            if !restored {
                self.rejection = Some("Add Object redo target is unavailable");
                return;
            }
            refresh_target_manifest(project, &self.target);
            return;
        }
        self.prev = None;
        self.rejection = None;
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = match scene_object_append_slot_for_target(
            project,
            &self.target,
            &self.catalog_default,
            &scope,
            render_id,
        ) {
            Ok(k) => k,
            Err(reason) => {
                self.rejection = Some(reason);
                return;
            }
        };
        if self.physics_body_metadata.is_some() {
            match self.physics_world_for_scope(project) {
                Ok(Some((world_id, body_slot))) => {
                    self.execute_physics(project, world_id, body_slot, k);
                    return;
                }
                Err(reason) => {
                    self.rejection = Some(reason);
                    return;
                }
                Ok(None) => {}
            }
        }
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
                        "node.pbr_material",
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
                    "node.pbr_material",
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
        self.capture_after(project);
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
        self.prev.is_some() && self.rejection.is_none()
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }
}

const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

/// Resolve the next physical render-scene object slot from content-owned
/// state. Callers may carry a logical UI count for action compatibility, but
/// it cannot identify a physical slot when a compound object has children.
pub(super) fn scene_object_append_slot(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    render_id: u32,
) -> Result<u32, &'static str> {
    let Some(render) = nodes.iter().find(|node| node.id == render_id) else {
        return Err("Add scene object render scene is unavailable");
    };
    if render.type_id != RENDER_SCENE_TYPE_ID {
        return Err("Add scene object target is not a render scene");
    }
    let Some(value) = render.params.get("objects") else {
        return Err("Add scene object render scene has an invalid object count");
    };
    let count = match value {
        SerializedParamValue::Float { value }
            if value.is_finite() && *value >= 0.0 && value.fract() == 0.0 =>
        {
            if *value >= u32::MAX as f32 {
                return Err("Add scene object object count is exhausted");
            }
            *value as u32
        }
        SerializedParamValue::Int { value } if *value >= 0 => *value as u32,
        _ => return Err("Add scene object render scene has an invalid object count"),
    };
    // Render-scene counts are written as f32. Reject an increment which
    // cannot survive serialization instead of reusing the last slot.
    if count == u32::MAX || (count + 1) as f32 as u32 != count + 1 {
        return Err("Add scene object object count is exhausted");
    }
    let destination = format!("object_{count}");
    if wires
        .iter()
        .any(|wire| wire.to_node == render_id && wire.to_port == destination)
    {
        return Err("Add scene object destination object slot is occupied");
    }
    Ok(count)
}

fn scene_object_append_slot_for_target(
    project: &Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    scope: &[u32],
    render_id: u32,
) -> Result<u32, &'static str> {
    let Some(def) = project.graph_for_target(target, Some(catalog_default)) else {
        return Err("Add scene object target is unavailable");
    };
    scene_object_append_slot_for_scope(def, scope, render_id)
}

pub(super) fn scene_object_append_slot_for_scope(
    def: &EffectGraphDef,
    scope: &[u32],
    render_id: u32,
) -> Result<u32, &'static str> {
    let Some((nodes, wires)) = graph_level(def, scope) else {
        return Err("Add scene object scope is unavailable");
    };
    scene_object_append_slot(nodes, wires, render_id)
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
                let light_id = max_node_id_over(&def.nodes).checked_add(1)?;
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
/// sequence of sub-steps. Ungrouped hand-built objects (a loose
/// `scene_object` whose mesh/transform/material producers are not wrapped in
/// a group) use the same exclusive upstream ownership walk as duplication,
/// retaining shared producers and their dependencies.
///
/// `object_index` (`k`, the 0-based slot in `object_k`) is resolved by the
/// caller from the live Vm's own `ObjectKnownRow::index`. This is a delete
/// target for an existing physical slot; append commands resolve their next
/// slot from the content-owned render-scene count at execution time.
#[derive(Debug)]
pub struct RemoveSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    object_index: u32,
    expected_source: Option<NodeId>,
    catalog_default: EffectGraphDef,
    rejection: Option<String>,
    /// The level and metadata before this edit, plus the host instance state
    /// that is pruned when the removed object's exposures disappear.
    prev: Option<RemovedSceneObjectSnapshot>,
    applied: bool,
}

#[derive(Debug, Clone)]
struct RemovedSceneObjectSnapshot {
    graph: Option<EffectGraphDef>,
    instance: InstanceLayerSnapshot,
    after_graph: EffectGraphDef,
    after_instance: InstanceLayerSnapshot,
}

#[derive(Debug, Clone)]
struct RemovedObjectSnapshot {
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    metadata: Option<PresetMetadata>,
    instance: InstanceLayerSnapshot,
}

impl RemoveSceneObjectCommand {
    pub fn with_expected_source(mut self, source: NodeId) -> Self {
        self.expected_source = Some(source);
        self
    }

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
            expected_source: None,
            catalog_default,
            rejection: None,
            prev: None,
            applied: false,
        }
    }
}

impl Command for RemoveSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.applied = false;
        if let Some(snapshot) = &self.prev {
            if project
                .graph_target_owner(&self.target)
                .map(|owner| &owner.graph)
                != Some(&snapshot.graph)
            {
                self.rejection =
                    Some("Remove Object redo rejected: graph changed since undo".into());
                return;
            }
            if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                *def = snapshot.after_graph.clone();
            })
            .is_none()
            {
                self.rejection = Some("Remove Object redo target is unavailable".into());
                return;
            }
            refresh_target_manifest(project, &self.target);
            if let Some(instance) = project.graph_target_owner_mut(&self.target) {
                snapshot.after_instance.clone().restore(instance);
            }
            self.applied = true;
            return;
        }
        let Some(def) = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .cloned()
        else {
            return;
        };
        if !scene_object_source_matches(Some(&def), &self.scope_path, self.render_scene_node_id,
            self.object_index, self.expected_source.as_ref())
        {
            self.rejection = Some("Remove Object rejected: selected object changed".into());
            return;
        }
        if deletion_breaks_explicit_modifier_target(
            &def,
            &self.scope_path,
            self.render_scene_node_id,
            self.object_index,
        ) {
            self.rejection = Some(
                "Object is explicitly targeted by a scene modifier; retarget or remove that modifier first".into(),
            );
            return;
        }
        let Some(previous_instance) = project
            .graph_target_owner_mut(&self.target)
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
            self.rejection = Some((*reason).into());
            return;
        }
        if matches!(
            physics_match.as_ref(),
            Some(PhysicsSceneObjectMatch::Valid(_))
        ) && !scope.is_empty()
        {
            self.rejection =
                Some("Remove Object physics ownership requires a root-level scene".into());
            return;
        }
        let Some(previous_graph) = project
            .graph_target_owner(&self.target)
            .map(|owner| owner.graph.clone())
        else {
            return;
        };
        let mut candidate = def;
        let producer_id = graph_level(&candidate, &scope)
            .and_then(|(_, wires)| object_producer_id(wires, render_id, k));
        let producer_is_group = producer_id.as_ref().is_some_and(|producer_id| {
            graph_level(&candidate, &scope)
                .and_then(|(nodes, _)| nodes.iter().find(|node| node.id == *producer_id))
                .is_some_and(|node| node.type_id == GROUP_TYPE_ID)
        });
        if scope.is_empty()
            && producer_is_group
            && let Err(reason) = fluid::disconnect_scene_object_fluid_roles(
                &mut candidate,
                producer_id.expect("producer id checked above"),
            )
        {
            self.rejection = Some(reason);
            return;
        }
        let result = (|| {
            let def = &mut candidate;
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;

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
                let field_port = if physics.copies {
                    "copies_acceleration".to_string()
                } else {
                    format!("body_acceleration_{}", physics.body_slot)
                };
                nodes.retain(|node| !owned.contains(&node.id));
                wires.retain(|wire| {
                    !(owned.contains(&wire.from_node)
                        || owned.contains(&wire.to_node)
                        || (wire.to_node == physics.world_id && wire.to_port == field_port))
                });
            } else {
                let owned = if producer.type_id == "node.scene_object" {
                    loose_scene_object_owned_ids(nodes, wires, producer_id)
                } else {
                    std::collections::HashSet::from([producer_id])
                };
                for node in nodes.iter().filter(|node| owned.contains(&node.id)) {
                    collect_node_ids(std::slice::from_ref(node), &mut removed_ids);
                }
                nodes.retain(|node| !owned.contains(&node.id));
                wires.retain(|wire| {
                    !(owned.contains(&wire.from_node)
                        || owned.contains(&wire.to_node)
                        || (wire.to_node == render_id
                            && removed_indices
                                .iter()
                                .any(|index| wire.to_port == format!("object_{index}"))))
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
            Some(removed_params)
        })();
        let Some(removed_param_ids) = result else {
            return;
        };
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            *def = candidate.clone();
        })
        .is_none()
        {
            return;
        }
        prune_scene_target_params(project, &self.target, &removed_param_ids);
        refresh_target_manifest(project, &self.target);
        self.prev = Some(RemovedSceneObjectSnapshot {
            graph: previous_graph,
            instance: previous_instance,
            after_graph: candidate,
            after_instance: InstanceLayerSnapshot::capture(
                project
                    .graph_target_owner_mut(&self.target)
                    .expect("validated scene owner"),
            ),
        });
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(snapshot) = &self.prev else {
            return;
        };
        restore_scene_owner_graph(project, &self.target, snapshot.graph.clone());
        refresh_target_manifest(project, &self.target);
        if let Some(instance) = project.graph_target_owner_mut(&self.target) {
            snapshot.instance.clone().restore(instance);
        }
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Remove Object"
    }

    fn was_applied(&self) -> bool {
        self.applied
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
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
        sources.push(scene_source_in_level(&group.nodes, &group.wires, child_id)?);
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
pub fn deep_clone_with_fresh_ids(
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
        // Group interface parameters address inner nodes by their display
        // handles. The deep clone deduplicates every child handle, so carry
        // the direct child's old→new map across the interface as well. Nested
        // groups perform the same rewrite in their own recursive clone.
        let handle_map: std::collections::HashMap<_, _> = group
            .nodes
            .iter()
            .zip(&new_nodes)
            .filter_map(|(old, new)| Some((old.handle.as_ref()?, new.handle.as_ref()?.clone())))
            .map(|(old, new)| (old.clone(), new))
            .collect();
        for param in &mut group.interface.params {
            if let Some(new_handle) = handle_map.get(&param.target_handle) {
                param.target_handle = new_handle.clone();
            }
        }
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

/// Remove exposure bindings whose stable target lives in a deleted object
/// subtree. Shared binding ids are retained when another target still uses
/// them (the importer deliberately fans out one outer control to many nodes).
/// Return only ids that no longer have any surviving binding so the host
/// manifest and its modulation collections can be pruned by the caller.
pub(super) fn prune_scene_object_metadata(
    def: &mut EffectGraphDef,
    removed: &[NodeId],
) -> Vec<String> {
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
/// their exclusive upstream chain. Shared producers remain in place and
/// incoming shared-source wires are copied to the cloned chain.
#[derive(Debug)]
pub struct DuplicateSceneObjectCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    source_index: u32,
    expected_source: Option<NodeId>,
    catalog_default: EffectGraphDef,
    /// The target owner's graph before this edit. `None` is meaningful: the
    /// command must restore an unmaterialized catalog graph on undo.
    prev_graph: Option<Option<EffectGraphDef>>,
    /// Cached successful candidate for redo, preserving all cloned ids and
    /// fluid route boundaries exactly.
    after_graph: Option<EffectGraphDef>,
    /// Live manifest/modulation state before and after the duplicate. A
    /// structural refresh intentionally rebuilds the manifest, so retaining
    /// these snapshots keeps authored values stable across undo/redo too.
    prev_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    rejection: Option<String>,
    applied: bool,
}

impl DuplicateSceneObjectCommand {
    pub fn with_expected_source(mut self, source: NodeId) -> Self {
        self.expected_source = Some(source);
        self
    }

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
            expected_source: None,
            catalog_default,
            prev_graph: None,
            after_graph: None,
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

        if let (Some(previous_graph), Some(after_graph)) =
            (self.prev_graph.as_ref(), self.after_graph.as_ref())
        {
            let current_graph = project
                .graph_target_owner(&self.target)
                .map(|owner| owner.graph.clone());
            if current_graph.as_ref() != Some(previous_graph) {
                self.rejection =
                    Some("Duplicate Object redo rejected: graph changed since undo".into());
                return;
            }
            if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                *def = after_graph.clone();
            })
            .is_none()
            {
                self.rejection = Some("Duplicate Object redo target is unavailable".into());
                return;
            }
            refresh_target_manifest(project, &self.target);
            if let (Some(snapshot), Some(instance)) = (
                self.after_instance.clone(),
                project.graph_target_owner_mut(&self.target),
            ) {
                snapshot.restore(instance);
            }
            self.applied = true;
            return;
        }

        if !scene_object_source_matches(
            project.graph_for_target(&self.target, Some(&self.catalog_default)),
            &scope, render_id, src_k, self.expected_source.as_ref(),
        ) {
            self.rejection = Some("Duplicate Object rejected: selected object changed".into());
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

        let Some(baseline) = project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .cloned()
        else {
            self.rejection = Some("Duplicate Object requires an existing graph".into());
            return;
        };
        let Some(previous_graph) = project
            .graph_target_owner(&self.target)
            .map(|owner| owner.graph.clone())
        else {
            self.rejection = Some("Duplicate Object target is unavailable".into());
            return;
        };
        let baseline_instance = project
            .graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        let append_range = (|| {
            let (nodes, wires) = graph_level(&baseline, &scope)
                .ok_or("Duplicate Object scene scope is unavailable")?;
            let next = scene_object_append_slot(nodes, wires, render_id)?;
            let source = object_producer_id(wires, render_id, src_k)
                .ok_or("Duplicate Object source object is unavailable")?;
            let count = match physics_match.as_ref() {
                Some(PhysicsSceneObjectMatch::Valid(physics)) => physics.render_indices.len(),
                _ => nodes
                    .iter()
                    .find(|node| node.id == source)
                    .filter(|node| node.type_id == "node.scene_object")
                    .map(|_| {
                        wires
                            .iter()
                            .filter(|wire| {
                                wire.from_node == source
                                    && wire.to_node == render_id
                                    && (wire.to_port == "object"
                                        || wire.to_port.starts_with("object_"))
                            })
                            .count()
                    })
                    .unwrap_or_else(|| group_render_indices(wires, render_id, source).len()),
            };
            let count =
                u32::try_from(count).map_err(|_| "Duplicate Object object count is exhausted")?;
            let end = next
                .checked_add(count)
                .filter(|end| *end as f32 as u32 == *end)
                .ok_or("Duplicate Object object count is exhausted")?;
            if count == 0
                || (next..end).any(|index| {
                    wires.iter().any(|wire| {
                        wire.to_node == render_id && wire.to_port == format!("object_{index}")
                    })
                })
            {
                return Err("Duplicate Object destination slots are unavailable");
            }
            Ok((next, end))
        })();
        let (new_k, new_count) = match append_range {
            Ok(range) => range,
            Err(reason) => {
                self.rejection = Some(reason.into());
                return;
            }
        };
        let mut candidate = baseline.clone();
        let mut node_id_map: Vec<(NodeId, NodeId)> = Vec::new();
        let mut cloned_group_id = None;
        let result = (|| {
            let def = &mut candidate;
            // Document ids and handles are global even when the edit is
            // addressed through a nested scope. Seed both allocators
            // from the full tree before borrowing the target level.
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut taken);
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;

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
                nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: new_count as f32,
                    },
                );
            } else {
                let source_id = object_producer_id(wires, render_id, src_k)?;
                let source_node = nodes.iter().find(|n| n.id == source_id)?.clone();
                if source_node.type_id == "node.scene_object" {
                    let owned = loose_scene_object_owned_ids(nodes, wires, source_id);
                    if owned.is_empty() {
                        return None;
                    }
                    let mut source_outputs: Vec<(u32, String)> = wires
                        .iter()
                        .filter_map(|wire| {
                            if wire.from_node != source_id || wire.to_node != render_id {
                                return None;
                            }
                            let index = if wire.to_port == "object" {
                                0
                            } else {
                                wire.to_port.strip_prefix("object_")?.parse().ok()?
                            };
                            Some((index, wire.from_port.clone()))
                        })
                        .collect();
                    source_outputs.sort_unstable();
                    source_outputs.dedup_by_key(|(index, _)| *index);
                    if source_outputs.is_empty() {
                        return None;
                    }

                    let cloned_handle = source_node.handle.as_ref().map(|handle| format!("{handle} 2"));
                    let mut clones = Vec::new();
                    let mut numeric_map = std::collections::HashMap::new();
                    let mut offset_applied = false;
                    for source in nodes.iter().filter(|node| owned.contains(&node.id)) {
                        let mut clone = deep_clone_with_fresh_ids(
                            source,
                            &mut next_id,
                            &mut taken,
                            &mut node_id_map,
                        );
                        if source.id == source_id {
                            clone.handle = cloned_handle.clone();
                            clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
                        }
                        if clone.type_id == "node.transform_3d" && !offset_applied {
                            offset_applied = true;
                            let cur = match clone.params.get("pos_x") {
                                Some(SerializedParamValue::Float { value }) => *value,
                                _ => 0.0,
                            };
                            clone.params.insert(
                                "pos_x".to_string(),
                                SerializedParamValue::Float { value: cur + 0.5 },
                            );
                        }
                        numeric_map.insert(source.id, clone.id);
                        clones.push(clone);
                    }
                    let clone_id = *numeric_map.get(&source_id)?;
                    let cloned_wires: Vec<_> = wires
                        .iter()
                        .filter_map(|wire| {
                            let to_node = numeric_map.get(&wire.to_node).copied()?;
                            let from_node = numeric_map
                                .get(&wire.from_node)
                                .copied()
                                .unwrap_or(wire.from_node);
                            Some(EffectGraphWire {
                                from_node,
                                from_port: wire.from_port.clone(),
                                to_node,
                                to_port: wire.to_port.clone(),
                            })
                        })
                        .collect();
                    nodes.extend(clones);
                    wires.extend(cloned_wires);
                    for (part, (_, from_port)) in source_outputs.iter().enumerate() {
                        wires.push(scene_build_wire(
                            clone_id,
                            from_port,
                            render_id,
                            &format!("object_{}", new_k + part as u32),
                        ));
                    }
                    nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: new_count as f32,
                        },
                    );
                } else {
                let mut source_outputs: Vec<(u32, String)> = wires
                    .iter()
                    .filter_map(|wire| {
                        if wire.from_node != source_id || wire.to_node != render_id {
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
                if source_node.type_id == GROUP_TYPE_ID && scope.is_empty() {
                    cloned_group_id = Some(clone_id);
                }
                if source_node.type_id == GROUP_TYPE_ID {
                    // Group inputs are shared upstream signals, such as World
                    // controls. Keep them connected to the copied boundary.
                    let incoming: Vec<_> = wires.iter()
                        .filter(|wire| wire.to_node == source_id)
                        .map(|wire| EffectGraphWire {
                            to_node: clone_id,
                            ..wire.clone()
                        })
                        .collect();
                    wires.extend(incoming);
                }
                nodes.push(clone);
                for (part, _) in source_outputs.iter().enumerate() {
                    wires.push(scene_build_wire(
                        clone_id,
                        &source_outputs[part].1,
                        render_id,
                        &format!("object_{}", new_k + part as u32),
                    ));
                }
                nodes.iter_mut().find(|n| n.id == render_id)?.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Float {
                        value: new_count as f32,
                    },
                );
                }
            }

            Some(())
        })();
        if result.is_none() {
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
        if let (Some(original_group), Some(cloned_group)) = (
            graph_level(&baseline, &scope)
                .and_then(|(_, wires)| object_producer_id(wires, render_id, src_k)),
            cloned_group_id,
        ) && let Err(reason) = fluid::duplicate_scene_object_fluid_roles(
            &mut candidate,
            original_group,
            cloned_group,
            &node_id_map,
        ) {
            self.rejection = Some(reason);
            return;
        }
        if !node_id_map.is_empty() {
            if let Some(meta) = candidate.preset_metadata.as_mut() {
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
            }
            clone_sections::clone_scene_bindings(&mut candidate, &node_id_map);
        }
        let values = match project.graph_target_owner(&self.target)
            .ok_or_else(|| "Duplicate Object parameter owner is unavailable".to_string())
            .and_then(|owner| clone_values::prepare(owner, &self.catalog_default, &self.target, &candidate, &node_id_map))
        {
            Ok(values) => values,
            Err(reason) => { self.rejection = Some(reason); return; }
        };
        let after_graph = candidate.clone();
        if with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            *def = after_graph.clone();
        })
        .is_none()
        {
            self.rejection = Some("Duplicate Object target is unavailable".into());
            return;
        }
        refresh_target_manifest(project, &self.target);
        self.prev_graph = Some(previous_graph);
        self.after_graph = Some(after_graph);
        self.prev_instance = baseline_instance;
        if let Some(instance) = project.graph_target_owner_mut(&self.target) {
            for (id, value) in values {
                instance.set_base_param_from_automation(&id, value);
            }
        }
        self.after_instance = project
            .graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous_graph) = self.prev_graph.clone() else {
            return;
        };
        if !self.applied {
            return;
        }
        restore_scene_owner_graph(project, &self.target, previous_graph);
        refresh_target_manifest(project, &self.target);
        if let (Some(snapshot), Some(instance)) = (
            self.prev_instance.clone(),
            project.graph_target_owner_mut(&self.target),
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
                let env_id = max_node_id_over(&def.nodes).checked_add(1)?;
                let prev_metadata = def.preset_metadata.clone();

                let (env_id, env_node_id, env_node_params, prev) = {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let prev = (nodes.clone(), wires.clone());

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
                let fog_id = max_node_id_over(&def.nodes).checked_add(1)?;
                let prev_metadata = def.preset_metadata.clone();

                let (fog_id, fog_node_id, prev) = {
                    let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                    let prev = (nodes.clone(), wires.clone());

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
                let xf_id = max_node_id_over(&def.nodes).checked_add(1)?;
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

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
