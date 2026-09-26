//! Assign an existing grouped scene object to a fluid domain.

use std::collections::{BTreeMap, HashSet};

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID,
    GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, InterfacePortDef, PresetMetadata,
    SerializedParamValue, StringBindingDef,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{SceneParamMetadata, stamp_scene_node_exposures_into};
use manifold_core::scene_modifier_preset::SceneNodeRef;

use crate::command::Command;

use super::super::restore_scene_owner_graph;
use super::super::{
    InstanceLayerSnapshot, collect_all_handles, dedup_handle, max_node_id_over,
    refresh_target_manifest, scene_build_node, scene_build_wire,
    with_target_graph_mut,
};

const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
const FLUID_TYPE_ID: &str = "node.fluid_surface";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
const CUBE_MESH_TYPE_ID: &str = "node.cube_mesh";
const GLTF_MESH_TYPE_ID: &str = "node.gltf_mesh_source";
const MAX_FLUID_ROLES: u32 = 64;

/// One undoable assignment of a whole standard scene object group to a fluid
/// domain. The command stores complete graph snapshots so rejected edits are
/// atomic and undo/redo restores stable ids, metadata, and interfaces.
#[derive(Debug)]
pub struct AssignSceneFluidRoleCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    object_index: u32,
    domain: SceneNodeRef,
    role: u32,
    role_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    prev: Option<EffectGraphDef>,
    after: Option<EffectGraphDef>,
    prev_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    prev_graph: Option<Option<EffectGraphDef>>,
    applied: bool,
    rejection: Option<String>,
}

impl AssignSceneFluidRoleCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        object_index: u32,
        domain: SceneNodeRef,
        role: u32,
        role_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            object_index,
            domain,
            role,
            role_metadata,
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

    fn current_snapshot(&self, project: &Project) -> Option<EffectGraphDef> {
        project
            .graph_for_target(&self.target, Some(&self.catalog_default))
            .cloned()
    }

    fn commit_snapshot(&self, project: &mut Project, snapshot: &EffectGraphDef) -> bool {
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
}

impl Command for AssignSceneFluidRoleCommand {
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
                    Some("Assign Fluid Role redo rejected: graph changed since undo".into());
                return;
            }
            if !self.commit_snapshot(project, after) {
                self.rejection = Some("Assign Fluid Role redo target is unavailable".into());
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
            self.rejection = Some("Assign Fluid Role requires an existing graph".into());
            return;
        };
        let Some(previous_graph) = self.owner_graph(project) else {
            self.rejection = Some("Assign Fluid Role target is unavailable".into());
            return;
        };
        let Some(baseline_instance) = project.graph_target_owner_mut(&self.target)
            .map(|instance| InstanceLayerSnapshot::capture(&*instance))
        else {
            self.rejection = Some("Assign Fluid Role target is unavailable".into());
            return;
        };

        let mut candidate = baseline.clone();
        let role_metadata = self.role_metadata.clone();
        let result = build_assignment(
            &mut candidate,
            self.render_scene_node_id,
            self.object_index,
            &self.domain,
            self.role,
            &role_metadata,
        );
        if let Err(reason) = result {
            self.rejection = Some(reason);
            return;
        }
        if !self.commit_snapshot(project, &candidate) {
            self.rejection = Some("Assign Fluid Role target is unavailable".into());
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
        let Some(previous_graph) = self.prev_graph.clone() else {
            return;
        };
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
        "Assign Fluid Role"
    }

    fn was_applied(&self) -> bool {
        self.applied
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

/// Pure eligibility used by the scene projection. It intentionally excludes
/// domain and destination-slot checks, which belong to the command's full
/// preflight because they depend on the selected fluid and current wires.
pub fn scene_fluid_role_eligibility(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
) -> Result<(), String> {
    discover_role_object(def, render_scene_node_id, object_index).map(|_| ())
}

fn build_assignment(
    def: &mut EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
    domain: &SceneNodeRef,
    role: u32,
    role_metadata: &[SceneParamMetadata],
) -> Result<(), String> {
    if role >= 4 {
        return Err(
            "Assign Fluid Role role must be one of Initial Fill, Inflow, Outflow, or Collider"
                .into(),
        );
    }
    let object = discover_role_object(def, render_scene_node_id, object_index)?;
    let (domain_scope, fluid_id) = resolve_domain(def, render_scene_node_id, domain)?;
    let role_port = first_free_role_port(def, &domain_scope, fluid_id)?;

    let mut next_id = max_node_id_over(&def.nodes)
        .checked_add(1)
        .ok_or_else(|| "Assign Fluid Role document id space is exhausted".to_string())?;
    let mut fresh_id = || {
        let id = next_id;
        next_id = next_id.checked_add(1)?;
        Some(id)
    };
    let role_id =
        fresh_id().ok_or_else(|| "Assign Fluid Role document id space is exhausted".to_string())?;
    let source_port = format!("fluid_role_source_{role_id}");
    let mut node_ids = Vec::new();
    super::super::collect_node_ids(&def.nodes, &mut node_ids);
    let mut stable_ids: HashSet<NodeId> = node_ids.into_iter().collect();
    let stable_id = |prefix: &str, doc_id: u32, ids: &mut HashSet<NodeId>| {
        let base = format!("{prefix}_{doc_id}");
        let mut candidate = NodeId::new(base.clone());
        let mut suffix = 2u32;
        while ids.contains(&candidate) {
            candidate = NodeId::new(format!("{base}_{suffix}"));
            suffix = suffix.saturating_add(1);
        }
        ids.insert(candidate.clone());
        candidate
    };
    let role_node_id = stable_id("fluid_role_source", role_id, &mut stable_ids);

    let mut handles = HashSet::new();
    collect_all_handles(&def.nodes, &mut handles);
    let handle = dedup_handle(&format!("{} Fluid Role", object.handle), &mut handles);
    let role_handle = dedup_handle(&format!("{handle} Source"), &mut handles);
    let role_params = role_params(&object, role)?;
    let mut role_node = scene_build_node(
        role_id,
        ROLE_SOURCE_TYPE_ID,
        Some(role_handle),
        role_params.clone(),
    );
    role_node.node_id = role_node_id.clone();

    let group = def
        .nodes
        .iter_mut()
        .find(|node| node.id == object.group_id)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or_else(|| "Assign Fluid Role selected object group is unavailable".to_string())?;
    let output_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|node| node.id)
        .ok_or_else(|| "Assign Fluid Role object group has no output boundary".to_string())?;
    let shared_transform = object.shared_transform_id;
    group.nodes.push(role_node);
    group.wires.push(scene_build_wire(
        shared_transform,
        "transform",
        role_id,
        "transform",
    ));
    for (slot, transform_id) in object.local_transforms.iter().enumerate() {
        if let Some(transform_id) = transform_id {
            group.wires.push(scene_build_wire(
                *transform_id,
                "transform",
                role_id,
                &format!("part_{slot}"),
            ));
        }
    }
    if let Some(output) = group
        .interface
        .outputs
        .iter()
        .find(|port| port.name == source_port)
    {
        if output.port_type != "FluidRole" {
            return Err("Assign Fluid Role object group has a conflicting role output port".into());
        }
    } else {
        group.interface.outputs.push(InterfacePortDef {
            name: source_port.clone(),
            port_type: "FluidRole".into(),
        });
    }
    group
        .wires
        .push(scene_build_wire(role_id, "role", output_id, &source_port));

    let string_binding =
        role_source_string_binding(def, object.sources.first(), role_node_id.clone());
    stamp_role_metadata(
        def,
        role_id,
        &role_node_id,
        &role_params,
        role_metadata,
        &handle,
    );
    if let Some(binding) = string_binding {
        def.preset_metadata
            .get_or_insert_with(empty_scene_metadata)
            .string_bindings
            .push(binding);
    }
    route_role_to_domain(
        &mut def.nodes,
        &mut def.wires,
        &domain_scope,
        object.producer_id,
        source_port,
        role_port,
        fluid_id,
        &mut fresh_id,
    )?;
    Ok(())
}

#[derive(Debug)]
struct RoleObject {
    producer_id: u32,
    group_id: u32,
    handle: String,
    sources: Vec<ImportedSource>,
    shared_transform_id: u32,
    local_transforms: Vec<Option<u32>>,
    cube_size: Option<f32>,
}

#[derive(Debug, Clone)]
struct ImportedSource {
    node_id: NodeId,
    params: BTreeMap<String, SerializedParamValue>,
}

fn discover_role_object(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
) -> Result<RoleObject, String> {
    let render = def
        .nodes
        .iter()
        .find(|node| node.id == render_scene_node_id)
        .ok_or_else(|| "Assign Fluid Role render scene is unavailable".to_string())?;
    if render.type_id != RENDER_SCENE_TYPE_ID {
        return Err("Assign Fluid Role target is not a render scene".into());
    }
    let producer_id = def
        .wires
        .iter()
        .find(|wire| {
            wire.to_node == render_scene_node_id && wire.to_port == format!("object_{object_index}")
        })
        .map(|wire| wire.from_node)
        .ok_or_else(|| "Assign Fluid Role selected scene object is unavailable".to_string())?;
    let producer = def
        .nodes
        .iter()
        .find(|node| node.id == producer_id)
        .ok_or_else(|| {
            "Assign Fluid Role selected scene object producer is unavailable".to_string()
        })?;
    if producer.type_id != GROUP_TYPE_ID {
        return Err("Assign Fluid Role requires a standard whole-object group".into());
    }
    let group = producer
        .group
        .as_deref()
        .ok_or_else(|| "Assign Fluid Role selected object group is malformed".to_string())?;
    if group.nodes.iter().any(|node| node.type_id == FLUID_TYPE_ID) {
        return Err("Assign Fluid Role cannot target a group containing a fluid surface".into());
    }
    if group
        .interface
        .inputs
        .iter()
        .any(|port| port.name == "pose")
        || group
            .nodes
            .iter()
            .any(|node| node.type_id == "node.physics_world" || node.type_id == "node.rigid_body")
    {
        return Err(
            "Assign Fluid Role does not support physics-bound or dynamic object poses".into(),
        );
    }
    let render_indices = group_render_indices(&def.wires, render_scene_node_id, producer_id);
    if render_indices.first().copied() != Some(object_index) {
        return Err(
            "Assign Fluid Role requires the whole object group, not an individual material child"
                .into(),
        );
    }
    let output = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .ok_or_else(|| "Assign Fluid Role object group has no output boundary".to_string())?;
    let outputs: Vec<_> = group
        .interface
        .outputs
        .iter()
        .filter(|port| port.port_type == "Object")
        .collect();
    if outputs.is_empty() {
        return Err("Assign Fluid Role object group has no object outputs".into());
    }
    let mut object_ids = Vec::new();
    for port in &outputs {
        let wire = group
            .wires
            .iter()
            .find(|wire| wire.to_node == output.id && wire.to_port == port.name)
            .ok_or_else(|| "Assign Fluid Role object group output is unwired".to_string())?;
        let object = group
            .nodes
            .iter()
            .find(|node| node.id == wire.from_node && node.type_id == "node.scene_object")
            .ok_or_else(|| {
                "Assign Fluid Role object group has an unsupported material or mesh chain"
                    .to_string()
            })?;
        object_ids.push(object.id);
    }
    let shared_transform_id = find_shared_transform(group, object_ids[0])?;
    let mut local_transforms = Vec::new();
    let mut sources = Vec::new();
    for object_id in &object_ids {
        let object = group
            .nodes
            .iter()
            .find(|node| node.id == *object_id)
            .unwrap();
        let transform_wire = group
            .wires
            .iter()
            .find(|wire| wire.to_node == object.id && wire.to_port == "transform")
            .or_else(|| {
                group
                    .wires
                    .iter()
                    .find(|wire| wire.to_node == object.id && wire.to_port == "parent_transform")
            });
        if object_ids.len() > 1 {
            let parent = group
                .wires
                .iter()
                .find(|wire| wire.to_node == object.id && wire.to_port == "parent_transform")
                .ok_or_else(|| {
                    "Assign Fluid Role compound part is missing its parent transform".to_string()
                })?;
            if parent.from_node != shared_transform_id || parent.from_port != "transform" {
                return Err(
                    "Assign Fluid Role compound parts must share one parent transform".into(),
                );
            }
        }
        let mut local_transform = None;
        if let Some(wire) = transform_wire
            && wire.to_port == "transform"
        {
            let transform = group
                .nodes
                .iter()
                .find(|node| node.id == wire.from_node && node.type_id == "node.transform_3d")
                .ok_or_else(|| "Assign Fluid Role child transform is malformed".to_string())?;
            if transform.id != shared_transform_id {
                local_transform = Some(transform.id);
            }
        }
        local_transforms.push(local_transform);
        let mut source = find_mesh_source(group, object.id)?;
        if let Some(source) = source.as_mut() {
            normalize_source_params(def, source)?;
        }
        sources.push(source);
    }
    if object_ids.len() > 64 {
        return Err("Assign Fluid Role supports at most 64 object parts".into());
    }
    if sources.iter().all(Option::is_none) && object_ids.len() > 1 {
        return Err("Assign Fluid Role does not support compound builtin cube groups".into());
    }
    let sources = if sources.iter().all(Option::is_some) {
        let sources: Vec<_> = sources.into_iter().flatten().collect();
        let first = sources.first().cloned().unwrap();
        for other in &sources[1..] {
            if !same_source_selector(&first, other) {
                return Err(
                    "Assign Fluid Role imported material selectors are inconsistent".into(),
                );
            }
        }
        sources
    } else if sources.iter().all(Option::is_none) {
        Vec::new()
    } else {
        return Err("Assign Fluid Role mixes builtin and imported object parts".into());
    };
    let cube_size = if sources.is_empty() {
        Some(cube_group_size(group, &object_ids).unwrap_or(1.0))
    } else {
        None
    };
    Ok(RoleObject {
        producer_id,
        group_id: producer_id,
        handle: producer
            .handle
            .clone()
            .unwrap_or_else(|| format!("Object {object_index}")),
        sources,
        shared_transform_id,
        local_transforms,
        cube_size,
    })
}

fn find_shared_transform(group: &GroupDef, object_id: u32) -> Result<u32, String> {
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
        .ok_or_else(|| "Assign Fluid Role requires an authored object transform".to_string())?;
    let node = group
        .nodes
        .iter()
        .find(|node| node.id == wire.from_node && node.type_id == "node.transform_3d")
        .ok_or_else(|| "Assign Fluid Role authored object transform is malformed".to_string())?;
    Ok(node.id)
}

fn find_mesh_source(group: &GroupDef, object_id: u32) -> Result<Option<ImportedSource>, String> {
    let wire = group
        .wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "vertices")
        .ok_or_else(|| "Assign Fluid Role object mesh input is missing".to_string())?;
    let node = group
        .nodes
        .iter()
        .find(|node| node.id == wire.from_node)
        .ok_or_else(|| "Assign Fluid Role object mesh source is unavailable".to_string())?;
    match node.type_id.as_str() {
        CUBE_MESH_TYPE_ID => Ok(None),
        GLTF_MESH_TYPE_ID => Ok(Some(ImportedSource {
            node_id: node.node_id.clone(),
            params: node.params.clone(),
        })),
        "node.gltf_skinned_mesh_source"
        | "node.skin_mesh"
        | "node.morph_targets_blend"
        | "node.gltf_morph_deltas_source" => {
            Err("Assign Fluid Role does not support skinned or GPU-deformed sources".into())
        }
        _ => Err("Assign Fluid Role requires a supported static mesh source".into()),
    }
}

fn cube_group_size(group: &GroupDef, object_ids: &[u32]) -> Option<f32> {
    let object_id = *object_ids.first()?;
    let wire = group
        .wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "vertices")?;
    let cube = group
        .nodes
        .iter()
        .find(|node| node.id == wire.from_node && node.type_id == CUBE_MESH_TYPE_ID)?;
    cube.params.get("size").and_then(as_number)
}

fn normalize_source_params(
    def: &EffectGraphDef,
    source: &mut ImportedSource,
) -> Result<(), String> {
    if matches!(source.params.get("path"), Some(SerializedParamValue::String { value }) if !value.is_empty())
    {
        return Ok(());
    }
    let Some(metadata) = def.preset_metadata.as_ref() else {
        return Err("Assign Fluid Role requires a bound glTF source path".into());
    };
    let Some(binding) = metadata.string_bindings.iter().find(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &source.node_id && param == "path")
    }) else {
        return Err("Assign Fluid Role requires a bound glTF source path".into());
    };
    if binding.default_value.is_empty() {
        return Err("Assign Fluid Role requires a bound glTF source path".into());
    }
    source.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: binding.default_value.clone(),
        },
    );
    Ok(())
}

fn same_source_selector(left: &ImportedSource, right: &ImportedSource) -> bool {
    [
        "path",
        "mesh_index",
        "primitive_index",
        "fit",
        "recenter",
        "translate_x",
        "translate_y",
        "translate_z",
        "fragment_count",
        "fragment_index",
    ]
    .iter()
    .all(|key| left.params.get(*key) == right.params.get(*key))
}

fn role_params(
    object: &RoleObject,
    role: u32,
) -> Result<BTreeMap<String, SerializedParamValue>, String> {
    let mut params = BTreeMap::new();
    params.insert("role".into(), SerializedParamValue::Enum { value: role });
    params.insert("enabled".into(), SerializedParamValue::Bool { value: true });
    params.insert("geometry".into(), SerializedParamValue::Enum { value: 1 });
    params.insert("shape".into(), SerializedParamValue::Enum { value: 1 });
    params.insert("radius".into(), float(1.0));
    params.insert("velocity_x".into(), float(0.0));
    params.insert("velocity_y".into(), float(0.0));
    params.insert("velocity_z".into(), float(0.0));
    params.insert("inherit_motion".into(), float(0.0));
    params.insert("friction".into(), float(0.0));
    params.insert("collider_parts".into(), int(32));
    if let Some(source) = object.sources.first() {
        for name in [
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
        ] {
            params.insert(
                name.into(),
                source
                    .params
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| source_default(name)),
            );
        }
        if object.sources.len() > 1 || object.local_transforms.iter().any(Option::is_some) {
            let rows = object
                .sources
                .iter()
                .enumerate()
                .map(|(slot, source)| {
                    vec![
                        slot as f32,
                        source
                            .params
                            .get("material_index")
                            .and_then(as_number)
                            .unwrap_or(-1.0),
                    ]
                })
                .collect();
            params.insert(
                "compound_materials".into(),
                SerializedParamValue::Table { rows },
            );
        }
    }
    if object.sources.is_empty() {
        let size = object.cube_size.unwrap_or(1.0);
        params.insert("radius".into(), float(size * 3.0_f32.sqrt() / 2.0));
    }
    Ok(params)
}

fn role_source_string_binding(
    def: &EffectGraphDef,
    source: Option<&ImportedSource>,
    role_node_id: NodeId,
) -> Option<StringBindingDef> {
    let source = source?;
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
                        node_id: role_node_id.clone(),
                        param: "path".into(),
                    },
                })
            }
            _ => None,
        })
}

fn stamp_role_metadata(
    def: &mut EffectGraphDef,
    role_id: u32,
    role_node_id: &NodeId,
    role_params: &BTreeMap<String, SerializedParamValue>,
    role_metadata: &[SceneParamMetadata],
    handle: &str,
) {
    let meta = def.preset_metadata.get_or_insert_with(empty_scene_metadata);
    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        role_id,
        role_node_id,
        ROLE_SOURCE_TYPE_ID,
        handle,
        role_metadata,
        role_params,
    );
}

fn resolve_domain(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    domain: &SceneNodeRef,
) -> Result<(Vec<u32>, u32), String> {
    if !def
        .nodes
        .iter()
        .any(|node| node.id == render_scene_node_id && node.type_id == RENDER_SCENE_TYPE_ID)
    {
        return Err("Assign Fluid Role render scene is unavailable".into());
    }
    resolve_domain_ref(def, domain)
}

pub(super) fn resolve_domain_ref(
    def: &EffectGraphDef,
    domain: &SceneNodeRef,
) -> Result<(Vec<u32>, u32), String> {
    let mut nodes = def.nodes.as_slice();
    let mut runtime_scope = Vec::with_capacity(domain.scope.len());
    for stable in &domain.scope {
        let group = nodes
            .iter()
            .find(|node| node.node_id == *stable && node.type_id == GROUP_TYPE_ID)
            .ok_or_else(|| "Assign Fluid Role domain scope is unavailable".to_string())?;
        runtime_scope.push(group.id);
        nodes = group
            .group
            .as_deref()
            .ok_or_else(|| "Assign Fluid Role domain scope group is malformed".to_string())?
            .nodes
            .as_slice();
    }
    let fluid = nodes
        .iter()
        .find(|node| node.node_id == domain.node && node.type_id == FLUID_TYPE_ID)
        .ok_or_else(|| "Assign Fluid Role domain must resolve to node.fluid_surface".to_string())?;
    Ok((runtime_scope, fluid.id))
}

fn first_free_role_port(
    def: &EffectGraphDef,
    scope: &[u32],
    fluid_id: u32,
) -> Result<String, String> {
    let (nodes, wires) = graph_level(def, scope)
        .ok_or_else(|| "Assign Fluid Role domain scope is unavailable".to_string())?;
    if !nodes
        .iter()
        .any(|node| node.id == fluid_id && node.type_id == FLUID_TYPE_ID)
    {
        return Err("Assign Fluid Role domain is unavailable".into());
    }
    (0..MAX_FLUID_ROLES)
        .map(|slot| format!("role_{slot}"))
        .find(|port| {
            !wires
                .iter()
                .any(|wire| wire.to_node == fluid_id && wire.to_port == *port)
        })
        .ok_or_else(|| "Assign Fluid Role fluid domain has no free role ports".into())
}

fn route_role_to_domain(
    nodes: &mut [EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    scope: &[u32],
    source_group_id: u32,
    source_port: String,
    target_port: String,
    fluid_id: u32,
    fresh_id: &mut impl FnMut() -> Option<u32>,
) -> Result<(), String> {
    if scope.is_empty() {
        wires.push(scene_build_wire(
            source_group_id,
            &source_port,
            fluid_id,
            &target_port,
        ));
        return Ok(());
    }
    let group_id = scope[0];
    let group = nodes
        .iter_mut()
        .find(|node| node.id == group_id && node.type_id == GROUP_TYPE_ID)
        .ok_or_else(|| "Assign Fluid Role domain boundary group is unavailable".to_string())?;
    let body = group
        .group
        .as_deref_mut()
        .ok_or_else(|| "Assign Fluid Role domain boundary group is malformed".to_string())?;
    if let Some(port) = body
        .interface
        .inputs
        .iter()
        .find(|port| port.name == source_port)
    {
        if port.port_type != "FluidRole" {
            return Err("Assign Fluid Role domain boundary has a conflicting role input".into());
        }
    } else {
        body.interface.inputs.push(InterfacePortDef {
            name: source_port.clone(),
            port_type: "FluidRole".into(),
        });
    }
    wires.push(scene_build_wire(
        source_group_id,
        &source_port,
        group_id,
        &source_port,
    ));
    let input_id = if let Some(node) = body
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_INPUT_TYPE_ID)
    {
        node.id
    } else {
        let id = fresh_id()
            .ok_or_else(|| "Assign Fluid Role document id space is exhausted".to_string())?;
        body.nodes.push(scene_build_node(
            id,
            GROUP_INPUT_TYPE_ID,
            None,
            BTreeMap::new(),
        ));
        id
    };
    if scope.len() == 1 {
        if !body
            .nodes
            .iter()
            .any(|node| node.id == fluid_id && node.type_id == FLUID_TYPE_ID)
        {
            return Err("Assign Fluid Role domain is unavailable inside its scope".into());
        }
        body.wires.push(scene_build_wire(
            input_id,
            &source_port,
            fluid_id,
            &target_port,
        ));
        return Ok(());
    }
    route_role_to_domain(
        &mut body.nodes,
        &mut body.wires,
        &scope[1..],
        input_id,
        source_port,
        target_port,
        fluid_id,
        fresh_id,
    )
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

fn group_render_indices(wires: &[EffectGraphWire], render_id: u32, group_id: u32) -> Vec<u32> {
    let mut indices: Vec<_> = wires
        .iter()
        .filter_map(|wire| {
            (wire.from_node == group_id && wire.to_node == render_id)
                .then(|| wire.to_port.strip_prefix("object_")?.parse().ok())
                .flatten()
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn source_default(name: &str) -> SerializedParamValue {
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
        _ => float(0.0),
    }
}

fn as_number(value: &SerializedParamValue) -> Option<f32> {
    match value {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        SerializedParamValue::Enum { value } => Some(*value as f32),
        _ => None,
    }
}

fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}

fn int(value: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value }
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

mod lifecycle;
pub(in crate::commands::graph::scene) use lifecycle::{
    disconnect_scene_object_fluid_roles, duplicate_scene_object_fluid_roles,
};
pub use lifecycle::{
    RemoveSceneFluidRoleCommand, RetargetSceneFluidRoleCommand, SceneFluidRoleAssignment,
    scene_fluid_role_assignments,
};
