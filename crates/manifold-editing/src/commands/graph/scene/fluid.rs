//! Add-a-fluid scene command.

use std::collections::{BTreeMap, HashMap, HashSet};

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef,
    PresetMetadata, SerializedParamValue,
};
use manifold_core::liquid_domain::liquid_domains_in;
use manifold_core::project::Project;
use manifold_core::scene_exposure::{SceneParamMetadata, stamp_scene_node_exposures_into};

use crate::command::Command;

use super::super::{
    InstanceLayerSnapshot, collect_node_ids, dedup_handle,
    refresh_target_manifest, scene_build_node, scene_build_wire,
    with_target_graph_mut,
};
use super::{collect_all_handles, max_node_id_over, restore_scene_owner_graph};

#[cfg(feature = "gpu-proofs")]
const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
#[cfg(feature = "gpu-proofs")]
const TRANSFORM_TYPE_ID: &str = "node.transform_3d";
#[cfg(feature = "gpu-proofs")]
const MATERIAL_TYPE_ID: &str = "node.pbr_material";
#[cfg(feature = "gpu-proofs")]
const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
const ID_SPACE_EXHAUSTED: &str = "Add Fluid document id space is exhausted";

mod template;
mod world_controls;

pub use template::{ExposureSet, LiquidTemplate, TemplateExposure};
#[cfg(feature = "gpu-proofs")]
pub use template::flip_scene_fluid_template;

type GraphSnapshot = EffectGraphDef;

/// Append one grouped liquid template to an existing root render scene.
///
/// All template outputs reserve slots from the live render_scene objects
/// parameter in one transaction. This preserves physical slot ordering for compound
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
    whitewater_metadata: Vec<SceneParamMetadata>,
    template: LiquidTemplate,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        fluid_metadata: Vec<SceneParamMetadata>,
        source_metadata: Vec<SceneParamMetadata>,
        material_metadata: Vec<SceneParamMetadata>,
        object_metadata: Vec<SceneParamMetadata>,
        template: LiquidTemplate,
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
            whitewater_metadata: Vec::new(),
            template,
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

    pub fn with_whitewater_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.whitewater_metadata = metadata;
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

/// Gives every node under `node`'s group body a fresh document id. Document
/// ids are unique across nesting, so a template's local ids cannot be kept.
fn renumber_nested(
    node: &mut EffectGraphNode,
    fresh_id: &mut dyn FnMut() -> Option<u32>,
) -> Result<(), &'static str> {
    let Some(body) = node.group.as_deref_mut() else {
        return Ok(());
    };
    let mut renamed = HashMap::new();
    for inner in &mut body.nodes {
        let id = fresh_id().ok_or(ID_SPACE_EXHAUSTED)?;
        renamed.insert(inner.id, id);
        inner.id = id;
        inner.node_id = NodeId::new(manifold_core::short_id());
        renumber_nested(inner, fresh_id)?;
    }
    for wire in &mut body.wires {
        let (Some(from), Some(to)) = (renamed.get(&wire.from_node), renamed.get(&wire.to_node))
        else {
            continue;
        };
        wire.from_node = *from;
        wire.to_node = *to;
    }
    Ok(())
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
        let template = &self.template;
        let metadata_for = |set: ExposureSet| match set {
            ExposureSet::Fluid => self.fluid_metadata(),
            ExposureSet::Domain => self.domain_metadata(),
            ExposureSet::SourceTransform => self.source_metadata(),
            ExposureSet::Role => self.role_metadata.clone(),
            ExposureSet::Material => self.material_metadata.clone(),
            ExposureSet::Object => self.object_metadata.clone(),
            ExposureSet::Whitewater => self.whitewater_metadata.clone(),
        };
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
            let output_count = u32::try_from(template.object_outputs.len())
                .map_err(|_| "Add Fluid object count is exhausted")?;
            if output_count == 0 || template.object_outputs.iter().collect::<HashSet<_>>().len() != template.object_outputs.len() {
                return Err("Add Fluid template outputs are malformed");
            }
            let Some(new_count) = object_count.checked_add(output_count) else {
                return Err("Add Fluid object count is exhausted");
            };
            for (offset, output) in template.object_outputs.iter().enumerate() {
                if template.wires.iter().filter(|wire| wire.to_node == template.output_node && wire.to_port == *output).count() != 1 {
                    return Err("Add Fluid template outputs are malformed");
                }
                let destination = format!("object_{}", object_count + offset as u32);
                if def.wires.iter().any(|wire| wire.to_node == render_id && wire.to_port == destination) {
                    return Err("Add Fluid destination object slot is occupied");
                }
            }

            let mut next_id = max_node_id_over(&def.nodes).checked_add(1);
            let mut fresh_id = || {
                let id = next_id?;
                next_id = id.checked_add(1);
                Some(id)
            };
            let mut group_id = 0;
            let mut body_nodes = template.nodes.clone();
            let mut renamed = HashMap::new();
            for (index, node) in body_nodes.iter_mut().enumerate() {
                if index == template.group_id_slot {
                    group_id = fresh_id().ok_or(ID_SPACE_EXHAUSTED)?;
                }
                let id = fresh_id().ok_or(ID_SPACE_EXHAUSTED)?;
                renamed.insert(node.id, id);
                node.id = id;
                renumber_nested(node, &mut fresh_id)?;
            }
            if template.group_id_slot >= body_nodes.len() {
                group_id = fresh_id().ok_or(ID_SPACE_EXHAUSTED)?;
            }
            let lookup = |local: u32| {
                renamed.get(&local).copied().ok_or("Add Fluid template is malformed")
            };
            lookup(template.output_node)?;
            let mut body_wires = Vec::with_capacity(template.wires.len());
            for wire in &template.wires {
                body_wires.push(scene_build_wire(
                    lookup(wire.from_node)?,
                    &wire.from_port,
                    lookup(wire.to_node)?,
                    &wire.to_port,
                ));
            }
            let domain = match liquid_domains_in(&body_nodes).as_slice() {
                [domain] => domain.clone(),
                _ => return Err("Add Fluid template must hold exactly one liquid domain"),
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
            let fluid_handle = next_fluid_handle(template.name_prefix, &mut handles);
            let group_handle = dedup_handle(&format!("{fluid_handle} Graph"), &mut handles);
            for node in &mut body_nodes {
                node.handle = match node.handle.take() {
                    Some(suffix) if suffix.is_empty() => Some(fluid_handle.clone()),
                    Some(suffix) => {
                        Some(dedup_handle(&format!("{fluid_handle} {suffix}"), &mut handles))
                    }
                    None => None,
                };
                node.node_id = stable_id(node.node_id.as_str(), node.id);
            }

            let mut exposed = Vec::with_capacity(template.exposures.len());
            let mut shared = Vec::new();
            for exposure in &template.exposures {
                let TemplateExposure::Node { node, set, section } = exposure else {
                    let TemplateExposure::Shared { spec, targets } = exposure else { unreachable!() };
                    if targets.is_empty() { return Err("Add Fluid shared exposure has no targets"); }
                    let mut spec = spec.as_ref().clone();
                    spec.id = format!("{group_id}_{}", spec.id);
                    spec.section = Some(match spec.section.as_deref() {
                        None | Some("Water") => fluid_handle.clone(),
                        Some(section) => format!("{fluid_handle} - {section}"),
                    });
                    let mut bindings = Vec::with_capacity(targets.len());
                    for (local, authored) in targets {
                        let fresh = lookup(*local)?;
                        let node = body_nodes.iter().find(|node| node.id == fresh)
                            .ok_or("Add Fluid shared exposure target is missing")?;
                        let BindingTarget::Node { param, .. } = &authored.target else {
                            return Err("Add Fluid shared exposure target is not a node");
                        };
                        let mut binding = authored.clone();
                        binding.id = spec.id.clone();
                        binding.target = BindingTarget::Node { node_id: node.node_id.clone(), param: param.clone() };
                        bindings.push(binding);
                    }
                    shared.push((spec, bindings));
                    continue;
                };
                let id = lookup(*node)?;
                let node = body_nodes
                    .iter()
                    .find(|node| node.id == id)
                    .ok_or("Add Fluid template is malformed")?;
                exposed.push((
                    id,
                    node.node_id.clone(),
                    node.type_id.clone(),
                    node.params.clone(),
                    *set,
                    *section,
                ));
            }

            let mut group =
                scene_build_node(group_id, GROUP_TYPE_ID, Some(group_handle), BTreeMap::new());
            group.node_id = stable_id("fluid_group", group_id);
            group.group = Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: Vec::new(),
                    outputs: template.object_outputs.iter().map(|name| InterfacePortDef {
                        name: name.clone(), port_type: "SceneObject".into(),
                    }).collect(),
                    params: Vec::new(),
                },
                nodes: body_nodes,
                wires: body_wires,
                tint: None,
            }));

            def.nodes.push(group);
            for (offset, output) in template.object_outputs.iter().enumerate() {
                def.wires.push(scene_build_wire(group_id, output, render_id,
                    &format!("object_{}", object_count + offset as u32)));
            }
            def.nodes
                .iter_mut()
                .find(|node| node.id == render_id)
                .expect("render scene validated above")
                .params
                .insert("objects".into(), float(new_count as f32));

            let meta = def.preset_metadata.get_or_insert_with(empty_scene_metadata);
            for (spec, bindings) in shared {
                meta.params.push(spec);
                meta.bindings.extend(bindings);
            }
            for (id, node_id, type_id, params, set, section) in &exposed {
                let label = match section {
                    Some(section) => format!("{fluid_handle} - {section}"),
                    None => fluid_handle.clone(),
                };
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    *id,
                    node_id,
                    type_id,
                    &label,
                    &metadata_for(*set),
                    params,
                );
            }
            world_controls::share_world_controls(def, group_id, &domain, &self.world_metadata)?;
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

#[cfg(feature = "gpu-proofs")]
fn int(value: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value }
}

fn next_fluid_handle(prefix: &str, handles: &mut HashSet<String>) -> String {
    let mut index = 1u32;
    loop {
        let candidate = format!("{prefix} {index}");
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
pub(super) use roles::{
    disconnect_scene_object_fluid_roles, duplicate_scene_object_fluid_roles,
    loose_scene_object_has_fluid_roles, remove_loose_scene_object_fluid_roles,
};
pub use roles::{
    AssignSceneFluidRoleCommand, RemoveSceneFluidRoleCommand, RetargetSceneFluidRoleCommand,
    SceneFluidRoleAssignment, restore_scene_object_fluid_roles, scene_fluid_role_assignments,
    scene_fluid_role_eligibility,
};
