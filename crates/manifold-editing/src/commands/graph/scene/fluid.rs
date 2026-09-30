//! Add-a-fluid scene command.

use std::collections::{BTreeMap, HashSet};

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{SceneParamMetadata, stamp_scene_node_exposures_into};

use crate::command::Command;

use super::super::{
    InstanceLayerSnapshot, collect_node_ids, dedup_handle,
    refresh_target_manifest, scene_build_node, scene_build_wire,
    with_target_graph_mut,
};
use super::{collect_all_handles, max_node_id_over, restore_scene_owner_graph};

/// Add Fluid authors FLIP until the default liquid template lands
/// (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P9 (Add Fluid authors the default
/// liquid template)).
const FLUID_TYPE_ID: &str = manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
const TRANSFORM_TYPE_ID: &str = "node.transform_3d";
const MATERIAL_TYPE_ID: &str = "node.pbr_material";
const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

mod world_controls;

type GraphSnapshot = EffectGraphDef;

/// Append one grouped liquid surface to an existing root render scene.
///
/// The object slot is resolved from the live render_scene objects parameter
/// at execution time. This preserves physical slot ordering for compound
/// scene objects, whose authored count includes every rendered part.
#[derive(Debug)]
pub struct AddSceneFluidCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    fluid_metadata: Vec<SceneParamMetadata>,
    source_metadata: Vec<SceneParamMetadata>,
    role_metadata: Vec<SceneParamMetadata>,
    world_metadata: Vec<SceneParamMetadata>,
    material_metadata: Vec<SceneParamMetadata>,
    object_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    prev: Option<GraphSnapshot>,
    after: Option<GraphSnapshot>,
    prev_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    prev_graph: Option<Option<EffectGraphDef>>,
    applied: bool,
    rejection: Option<String>,
}

impl AddSceneFluidCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        fluid_metadata: Vec<SceneParamMetadata>,
        source_metadata: Vec<SceneParamMetadata>,
        material_metadata: Vec<SceneParamMetadata>,
        object_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            fluid_metadata,
            source_metadata,
            role_metadata: Vec::new(),
            world_metadata: Vec::new(),
            material_metadata,
            object_metadata,
            catalog_default,
            prev: None,
            after: None,
            prev_instance: None,
            after_instance: None,
            prev_graph: None,
            applied: false,
            rejection: None,
        }
    }

    pub fn with_role_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.role_metadata = metadata;
        self
    }

    pub fn with_world_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.world_metadata = metadata;
        self
    }

    fn source_metadata(&self) -> Vec<SceneParamMetadata> {
        self.source_metadata
            .iter()
            .filter(|metadata| {
                matches!(
                    metadata.name.as_str(),
                    "pos_x" | "pos_y" | "pos_z" | "rot_x" | "rot_y" | "rot_z"
                        | "scale_x" | "scale_y" | "scale_z"
                )
            })
            .cloned()
            .collect()
    }

    fn domain_metadata(&self) -> Vec<SceneParamMetadata> {
        self.source_metadata
            .iter()
            .filter_map(|metadata| {
                let label = match metadata.name.as_str() {
                    "pos_x" | "pos_y" | "pos_z" => None,
                    "scale_x" => Some("Width"),
                    "scale_y" => Some("Height"),
                    "scale_z" => Some("Depth"),
                    _ => return None,
                };
                let mut metadata = metadata.clone();
                if let Some(label) = label {
                    metadata.label = label.to_string();
                }
                if metadata.name.starts_with("scale_") {
                    metadata.min = 0.5;
                    metadata.max = 20.0;
                    metadata.is_angle = false;
                }
                Some(metadata)
            })
            .collect()
    }

    fn fluid_metadata(&self) -> Vec<SceneParamMetadata> {
        self.fluid_metadata
            .iter()
            .filter(|metadata| {
                !world_controls::is_shared_fluid_control(&metadata.name) && !matches!(
                    metadata.name.as_str(),
                    "domain_size" | "emission" | "inflow_speed"
                )
            })
            .cloned()
            .collect()
    }

    fn current_snapshot(&self, project: &Project) -> Option<GraphSnapshot> {
        project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .cloned()
    }

    fn commit_snapshot(&self, project: &mut Project, snapshot: &GraphSnapshot) -> bool {
        with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            *def = snapshot.clone();
        })
        .is_some()
    }

    fn owner_graph(&self, project: &Project) -> Option<Option<EffectGraphDef>> {
        project
            .graph_target_owner(&self.target)
            .map(|owner| owner.graph.clone())
    }

    fn restore_original_graph(&self, project: &mut Project) -> bool {
        let Some(graph) = self.prev_graph.clone() else {
            return false;
        };
        restore_scene_owner_graph(project, &self.target, graph);
        true
    }
}

impl Command for AddSceneFluidCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.applied = false;

        if let (Some(before), Some(after)) = (self.prev.as_ref(), self.after.as_ref()) {
            if self.current_snapshot(project).as_ref() != Some(before)
                || self.owner_graph(project).as_ref() != self.prev_graph.as_ref()
            {
                self.rejection =
                    Some("Add Fluid redo rejected: graph changed since undo".to_string());
                return;
            }
            if !self.commit_snapshot(project, after) {
                self.rejection = Some("Add Fluid redo target is unavailable".to_string());
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

        let Some(baseline) = self.current_snapshot(project) else {
            self.rejection = Some("Add Fluid requires an existing graph".to_string());
            return;
        };
        let Some(previous_graph) = self.owner_graph(project) else {
            self.rejection = Some("Add Fluid target is unavailable".to_string());
            return;
        };
        let Some(baseline_instance) = project.graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance))
        else {
            self.rejection = Some("Add Fluid target is unavailable".to_string());
            return;
        };

        let render_id = self.render_scene_node_id;
        let fluid_metadata = self.fluid_metadata();
        let source_metadata = self.source_metadata();
        let domain_metadata = self.domain_metadata();
        let mut candidate = baseline.clone();
        let result = (|def: &mut EffectGraphDef| {
            let Some(render) = def.nodes.iter().find(|node| node.id == render_id) else {
                return Err("Add Fluid render scene is unavailable");
            };
            if render.type_id != RENDER_SCENE_TYPE_ID {
                return Err("Add Fluid target is not a render scene");
            }
            let Some(object_count) = render.params.get("objects").and_then(object_count) else {
                return Err("Add Fluid render scene has an invalid object count");
            };
            let Some(new_count) = object_count.checked_add(1) else {
                return Err("Add Fluid object count is exhausted");
            };
            let destination = format!("object_{object_count}");
            if def
                .wires
                .iter()
                .any(|wire| wire.to_node == render_id && wire.to_port == destination)
            {
                return Err("Add Fluid destination object slot is occupied");
            }

            let mut next_id = max_node_id_over(&def.nodes).checked_add(1);
            let mut fresh_id = || {
                let id = next_id?;
                next_id = id.checked_add(1);
                Some(id)
            };
            let Some(fluid_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(source_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(material_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(object_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(output_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(group_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(role_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };
            let Some(domain_id) = fresh_id() else {
                return Err("Add Fluid document id space is exhausted");
            };

            let mut existing_node_ids = Vec::new();
            collect_node_ids(&def.nodes, &mut existing_node_ids);
            let mut node_ids: HashSet<NodeId> = existing_node_ids.into_iter().collect();
            let mut stable_id = |prefix: &str, doc_id: u32| {
                let base = format!("{prefix}_{doc_id}");
                let mut candidate = NodeId::new(base.clone());
                let mut suffix = 2u32;
                while node_ids.contains(&candidate) {
                    candidate = NodeId::new(format!("{base}_{suffix}"));
                    suffix += 1;
                }
                node_ids.insert(candidate.clone());
                candidate
            };

            let mut handles = HashSet::new();
            collect_all_handles(&def.nodes, &mut handles);
            let fluid_handle = next_fluid_handle(&mut handles);
            let group_handle = dedup_handle(&format!("{fluid_handle} Graph"), &mut handles);
            let fluid_node_handle =
                dedup_handle(&format!("{fluid_handle} Simulation"), &mut handles);
            let source_handle = dedup_handle(&format!("{fluid_handle} Source"), &mut handles);
            let role_handle = dedup_handle(&format!("{fluid_handle} Source Role"), &mut handles);
            let domain_handle = dedup_handle(&format!("{fluid_handle} Domain"), &mut handles);
            let material_handle = dedup_handle(&format!("{fluid_handle} Material"), &mut handles);
            let object_handle = fluid_handle.clone();

            let mut fluid_params = BTreeMap::new();
            fluid_params.insert("domain_size".into(), float(4.0));
            fluid_params.insert("fill_height".into(), float(0.4));
            fluid_params.insert("resolution".into(), int(16));
            fluid_params.insert("whitewater".into(), float(0.0));
            fluid_params.insert("gravity".into(), float(-9.81));
            fluid_params.insert("emission".into(), float(0.0));
            fluid_params.insert("inflow_speed".into(), float(1.0));
            fluid_params.insert("speed".into(), float(1.0));
            fluid_params.insert("surface_subdivisions".into(), int(0));

            let mut source_params = BTreeMap::new();
            source_params.insert("pos_x".into(), float(0.0));
            source_params.insert("pos_y".into(), float(2.8));
            source_params.insert("pos_z".into(), float(0.0));
            source_params.insert("rot_x".into(), float(0.0));
            source_params.insert("rot_y".into(), float(0.0));
            source_params.insert("rot_z".into(), float(0.0));
            source_params.insert("scale_x".into(), float(0.7));
            source_params.insert("scale_y".into(), float(0.5));
            source_params.insert("scale_z".into(), float(0.7));

            let mut domain_params = BTreeMap::new();
            domain_params.insert("pos_x".into(), float(0.0));
            domain_params.insert("pos_y".into(), float(2.0));
            domain_params.insert("pos_z".into(), float(0.0));
            domain_params.insert("rot_x".into(), float(0.0));
            domain_params.insert("rot_y".into(), float(0.0));
            domain_params.insert("rot_z".into(), float(0.0));
            domain_params.insert("scale_x".into(), float(4.0));
            domain_params.insert("scale_y".into(), float(4.0));
            domain_params.insert("scale_z".into(), float(4.0));

            let mut material_params = BTreeMap::new();
            material_params.insert("color_r".into(), float(0.8));
            material_params.insert("color_g".into(), float(0.95));
            material_params.insert("color_b".into(), float(1.0));
            material_params.insert("roughness".into(), float(0.08));
            material_params.insert("transmission".into(), float(1.0));
            material_params.insert("ior".into(), float(1.333));
            material_params.insert("volume_geometry".into(), float(1.0));
            material_params.insert("volume_attenuation_color_r".into(), float(0.6));
            material_params.insert("volume_attenuation_color_g".into(), float(0.85));
            material_params.insert("volume_attenuation_color_b".into(), float(0.95));
            material_params.insert("volume_attenuation_distance".into(), float(2.0));

            let mut fluid = scene_build_node(
                fluid_id,
                FLUID_TYPE_ID,
                Some(fluid_node_handle),
                fluid_params,
            );
            fluid.node_id = stable_id("fluid_surface", fluid_id);
            let fluid_node_id = fluid.node_id.clone();
            let fluid_params = fluid.params.clone();

            let mut source = scene_build_node(
                source_id,
                TRANSFORM_TYPE_ID,
                Some(source_handle),
                source_params,
            );
            source.node_id = stable_id("fluid_source", source_id);
            let source_node_id = source.node_id.clone();
            let source_params = source.params.clone();

            let mut role_params = BTreeMap::new();
            role_params.insert("role".into(), SerializedParamValue::Enum { value: 1 });
            role_params.insert("enabled".into(), SerializedParamValue::Bool { value: true });
            role_params.insert("geometry".into(), SerializedParamValue::Enum { value: 0 });
            role_params.insert("shape".into(), SerializedParamValue::Enum { value: 1 });
            role_params.insert("radius".into(), float(3.0_f32.sqrt() / 2.0));
            role_params.insert("velocity_x".into(), float(0.0));
            role_params.insert("velocity_y".into(), float(-1.0));
            role_params.insert("velocity_z".into(), float(0.0));
            role_params.insert("inherit_motion".into(), float(0.0));
            role_params.insert("friction".into(), float(0.0));
            role_params.insert("collider_parts".into(), int(32));

            let mut role = scene_build_node(
                role_id,
                ROLE_SOURCE_TYPE_ID,
                Some(role_handle),
                role_params,
            );
            role.node_id = stable_id("fluid_role_source", role_id);
            let role_node_id = role.node_id.clone();
            let role_params = role.params.clone();

            let mut domain = scene_build_node(
                domain_id,
                TRANSFORM_TYPE_ID,
                Some(domain_handle),
                domain_params,
            );
            domain.node_id = stable_id("fluid_domain", domain_id);
            let domain_node_id = domain.node_id.clone();
            let domain_params = domain.params.clone();

            let mut material = scene_build_node(
                material_id,
                MATERIAL_TYPE_ID,
                Some(material_handle),
                material_params,
            );
            material.node_id = stable_id("fluid_material", material_id);
            let material_node_id = material.node_id.clone();
            let material_params = material.params.clone();

            let mut object = scene_build_node(
                object_id,
                SCENE_OBJECT_TYPE_ID,
                Some(object_handle),
                BTreeMap::new(),
            );
            object.node_id = stable_id("fluid_object", object_id);
            let object_node_id = object.node_id.clone();

            let mut output =
                scene_build_node(output_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());
            output.node_id = stable_id("fluid_output", output_id);

            let mut group =
                scene_build_node(group_id, GROUP_TYPE_ID, Some(group_handle), BTreeMap::new());
            group.node_id = stable_id("fluid_group", group_id);
            group.group = Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: Vec::new(),
                    outputs: vec![InterfacePortDef {
                        name: "object".into(),
                        port_type: "Object".into(),
                    }],
                    params: Vec::new(),
                },
                nodes: vec![fluid, source, role, domain, material, object, output],
                wires: vec![
                    scene_build_wire(source_id, "transform", role_id, "transform"),
                    scene_build_wire(domain_id, "transform", fluid_id, "domain"),
                    scene_build_wire(role_id, "role", fluid_id, "role_0"),
                    scene_build_wire(fluid_id, "vertices", object_id, "vertices"),
                    scene_build_wire(material_id, "out", object_id, "material"),
                    scene_build_wire(object_id, "object", output_id, "object"),
                ],
                tint: None,
            }));

            def.nodes.push(group);
            def.wires.push(scene_build_wire(
                group_id,
                "object",
                render_id,
                &destination,
            ));
            def.nodes
                .iter_mut()
                .find(|node| node.id == render_id)
                .expect("render scene validated above")
                .params
                .insert("objects".into(), float(new_count as f32));

            let meta = def.preset_metadata.get_or_insert_with(empty_scene_metadata);
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                fluid_id,
                &fluid_node_id,
                FLUID_TYPE_ID,
                &format!("{fluid_handle} - Simulation"),
                &fluid_metadata,
                &fluid_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                domain_id,
                &domain_node_id,
                TRANSFORM_TYPE_ID,
                &format!("{fluid_handle} - Domain"),
                &domain_metadata,
                &domain_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                source_id,
                &source_node_id,
                TRANSFORM_TYPE_ID,
                &format!("{fluid_handle} - Source Transform"),
                &source_metadata,
                &source_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                role_id,
                &role_node_id,
                ROLE_SOURCE_TYPE_ID,
                &format!("{fluid_handle} - Source"),
                &self.role_metadata,
                &role_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                material_id,
                &material_node_id,
                MATERIAL_TYPE_ID,
                &format!("{fluid_handle} - Material"),
                &self.material_metadata,
                &material_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                object_id,
                &object_node_id,
                SCENE_OBJECT_TYPE_ID,
                &fluid_handle,
                &self.object_metadata,
                &BTreeMap::new(),
            );
            world_controls::share_world_controls(def, group_id, fluid_id, &self.world_metadata)?;
            Ok(())
        })(&mut candidate);

        if let Err(reason) = result {
            self.rejection = Some(reason.to_string());
            return;
        }

        if !self.commit_snapshot(project, &candidate) {
            self.rejection = Some("Add Fluid target is unavailable".to_string());
            return;
        }

        refresh_target_manifest(project, &self.target);
        self.prev_graph = Some(previous_graph);
        self.prev_instance = Some(baseline_instance);
        self.after_instance = project.graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance));
        self.prev = Some(baseline);
        self.after = Some(candidate);
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(_previous) = self.prev.as_ref() else {
            return;
        };
        if !self.restore_original_graph(project) {
            return;
        }
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
        "Add Fluid"
    }

    fn was_applied(&self) -> bool {
        self.applied
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

fn object_count(value: &SerializedParamValue) -> Option<u32> {
    let value = match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        _ => return None,
    };
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= u32::MAX as f32 {
        return None;
    }
    Some(value as u32)
}

fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}

fn int(value: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value }
}

fn next_fluid_handle(handles: &mut HashSet<String>) -> String {
    let mut index = 1u32;
    loop {
        let candidate = format!("Fluid {index}");
        if !handles.contains(&candidate) {
            handles.insert(candidate.clone());
            return candidate;
        }
        index += 1;
    }
}

fn empty_scene_metadata() -> PresetMetadata {
    PresetMetadata {
        id: manifold_core::PresetTypeId::from_string("UnnamedScene".into()),
        display_name: "Scene".into(),
        category: "Geometry".into(),
        osc_prefix: "scene".into(),
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
}

#[cfg(test)]
mod tests;

mod roles;
pub(super) use roles::{disconnect_scene_object_fluid_roles, duplicate_scene_object_fluid_roles};
pub use roles::{
    AssignSceneFluidRoleCommand, RemoveSceneFluidRoleCommand, RetargetSceneFluidRoleCommand,
    SceneFluidRoleAssignment, restore_scene_object_fluid_roles, scene_fluid_role_assignments,
    scene_fluid_role_eligibility,
};
