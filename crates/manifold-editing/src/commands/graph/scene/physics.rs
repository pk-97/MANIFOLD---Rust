//! Physics scene command helpers extracted from `scene.rs`.

use super::*;

pub(super) fn append_physics_scene_object(
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
/// The source and authored-transform facts needed by standard scene-object
/// physics authoring. Keeping this discovery local to editing is
/// deliberate: the renderer VM is a read model, while commands must validate
/// the graph again on the content thread before changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScenePhysicsSourceKind {
    Imported,
    Builtin,
}

#[derive(Debug, Clone)]
pub(super) struct ScenePhysicsSource {
    /// Document id of the mesh producer.  Builtin sources are wired directly
    /// into `node.rigid_body.source`; imported sources use the body's copied
    /// selection parameters instead.
    pub(super) source_id: u32,
    pub(super) node_id: NodeId,
    pub(super) params: BTreeMap<String, SerializedParamValue>,
    pub(super) kind: ScenePhysicsSourceKind,
}

#[derive(Debug, Clone)]
pub(super) struct SceneObjectParts {
    pub(super) producer_id: u32,
    pub(super) object_id: u32,
    pub(super) group_id: Option<u32>,
    pub(super) authored_transform_id: u32,
    pub(super) source: ScenePhysicsSource,
    /// Every retained static compound source in render order.  The first
    /// entry is also `source`; keeping the complete list lets the rigid body
    /// author a stable `compound_materials` selector table instead of
    /// collapsing a multi-material asset to the primary material.
    pub(super) compound_sources: Vec<ScenePhysicsSource>,
    pub(super) object_handle: String,
    pub(super) render_indices: Vec<u32>,
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

pub(super) fn source_param_default(name: &str) -> SerializedParamValue {
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

pub(super) fn object_node_in_group(group: &GroupDef) -> Option<u32> {
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

pub(super) fn object_node_for_group_output(group: &GroupDef, output_port: &str) -> Option<u32> {
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

pub(super) fn group_output_port_for_render_wire(wire: &EffectGraphWire) -> Option<&str> {
    (wire.from_port == "object" || wire.from_port.starts_with("object_")).then_some(wire.from_port.as_str())
}

pub(super) fn authored_transform_in_level(
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
pub(super) fn group_authored_transform_in_level(
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
/// modifiers and transparent groups until its glTF or builtin mesh source.
/// Skinned and otherwise GPU-deformed sources are rejected before mutation
/// because their rendered geometry is not a stable standard Box3D collider
/// source.
pub(super) fn scene_source_in_level(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    object_id: u32,
) -> Result<ScenePhysicsSource, &'static str> {
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
                return Ok(ScenePhysicsSource {
                    source_id: node.id,
                    node_id: node.node_id.clone(),
                    params: node.params.clone(),
                    kind: ScenePhysicsSourceKind::Imported,
                });
            }
            "node.cube_mesh" | "node.platonic_solid_mesh" => {
                if scope_is_group {
                    return Err(
                        "Enable Physics builtin mesh source is hidden in a nested mesh group without a direct source route",
                    );
                }
                return Ok(ScenePhysicsSource {
                    source_id: node.id,
                    node_id: node.node_id.clone(),
                    params: node.params.clone(),
                    kind: ScenePhysicsSourceKind::Builtin,
                });
            }
            "node.gltf_skinned_mesh_source"
            | "node.skin_mesh"
            | "node.morph_targets_blend"
            | "node.gltf_morph_deltas_source" => {
                return Err("Enable Physics does not support skinned or GPU-deformed sources");
            }
            _ => return Err("Enable Physics requires a supported static mesh source"),
        }
    }
    Err("Enable Physics requires a supported static mesh source")
}

pub(super) fn scene_object_parts(
    def: &EffectGraphDef,
    render_id: u32,
    object_index: u32,
) -> Result<SceneObjectParts, &'static str> {
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
        let source = scene_source_in_level(&group.nodes, &group.wires, object_id)?;
        let mut compound_sources = Vec::new();
        for port in group.interface.outputs.iter().filter(|port| port.port_type == "Object") {
            let Some(part_id) = object_node_for_group_output(group, &port.name) else {
                return Err("Selected scene object group has an unsupported material or mesh chain");
            };
            compound_sources.push(scene_source_in_level(&group.nodes, &group.wires, part_id)?);
        }
        if compound_sources.is_empty() {
            return Err("Selected scene object group has no material sources");
        }
        let object = group
            .nodes
            .iter()
            .find(|node| node.id == object_id)
            .ok_or("Selected scene object is unavailable")?;
        return Ok(SceneObjectParts {
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
    let source = scene_source_in_level(&def.nodes, &def.wires, producer_id)?;
    let compound_sources = vec![source.clone()];
    Ok(SceneObjectParts {
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

pub(super) fn scene_body_params(
    source: &ScenePhysicsSource,
    def: &EffectGraphDef,
) -> Result<BTreeMap<String, SerializedParamValue>, &'static str> {
    if source.kind == ScenePhysicsSourceKind::Builtin {
        return Ok(BTreeMap::new());
    }
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

pub(super) fn compound_materials_param(
    sources: &[ScenePhysicsSource],
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

pub(super) fn source_string_binding(
    def: &EffectGraphDef,
    source: &ScenePhysicsSource,
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

pub(super) fn fresh_scene_node(
    id: u32,
    type_id: &str,
    handle: Option<String>,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
    scene_build_node(id, type_id, handle, params)
}

pub(super) fn group_pose_input_id(
    group: &GroupDef,
    object_id: u32,
) -> Result<u32, &'static str> {
    let mut pose_wires = group.wires.iter().filter(|wire| {
        wire.to_node == object_id
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
            && wire.from_port == "pose"
    });
    let pose_wire = pose_wires
        .next()
        .ok_or("Physics group pose input is missing")?;
    if pose_wires.next().is_some() {
        return Err("Physics group pose input is ambiguous");
    }
    if group.wires.iter().filter(|wire| {
        wire.to_node == object_id && wire.to_port == pose_wire.to_port
    }).count() != 1 {
        return Err("Physics group pose input is ambiguous");
    }
    let input = group
        .nodes
        .iter()
        .find(|node| node.id == pose_wire.from_node)
        .ok_or("Physics group pose input is unavailable")?;
    if input.type_id != GROUP_INPUT_TYPE_ID {
        return Err("Physics group pose source has the wrong type");
    }
    Ok(input.id)
}

pub(super) fn add_group_physics(
    group: &mut GroupDef,
    body_id: u32,
    body_params: BTreeMap<String, SerializedParamValue>,
    body_handle: String,
    input_id: u32,
    _output_id: u32,
    authored_transform_id: u32,
    object_id: u32,
    source_id: Option<u32>,
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
    let (input_node_id, input_node_actual) = if let Some(input) = group.nodes.iter()
        .find(|node| node.type_id == GROUP_INPUT_TYPE_ID)
    {
        (input.node_id.clone(), input.id)
    } else {
        let input = fresh_scene_node(input_id, GROUP_INPUT_TYPE_ID, None, BTreeMap::new());
        let identity = (input.node_id.clone(), input.id);
        group.nodes.push(input);
        identity
    };
    let output_id = group
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
        .unwrap()
        .id;
    group.nodes.push(body);
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
            wire.from_node = input_node_actual;
            wire.from_port = "pose".into();
        }
    }
    group.wires.push(scene_build_wire(
        authored_transform_id,
        "transform",
        body_id,
        "transform",
    ));
    if let Some(source_id) = source_id {
        group.wires.push(scene_build_wire(source_id, "source", body_id, "source"));
    }
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

pub(super) fn remove_group_physics(
    group: &mut GroupDef,
    body_id: u32,
    object_id: u32,
    authored_transform_id: u32,
) -> Result<(), &'static str> {
    let input_id = group_pose_input_id(group, object_id)?;
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
    let input_ids: std::collections::HashSet<_> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == GROUP_INPUT_TYPE_ID)
        .map(|node| node.id)
        .collect();
    for wire in &mut group.wires {
        if input_ids.contains(&wire.from_node) && wire.from_port == "pose"
        {
            wire.from_node = authored_transform_id;
            wire.from_port = "transform".into();
        }
    }
    group
        .wires
        .retain(|wire| wire.from_node != body_id && wire.to_node != body_id);
    group.nodes.retain(|node| {
        node.id != body_id
            && (node.id != input_id
                || group.wires.iter().any(|wire| {
                    wire.from_node == input_id || wire.to_node == input_id
                }))
    });
    // Remove the boundary only when it has no other non-object output; the
    // imported object shape has exactly one output and this keeps malformed
    // hand-authored groups from losing unrelated ports.
    let body_output_used = group
        .wires
        .iter()
        .any(|wire| wire.to_node == output_id && wire.to_port == "body");
    if !body_output_used {
        let pose_input_used = group.wires.iter().any(|wire| {
            wire.from_port == "pose"
                && group
                    .nodes
                    .iter()
                    .any(|node| node.id == wire.from_node && node.type_id == GROUP_INPUT_TYPE_ID)
        });
        if !pose_input_used {
            group.interface.inputs.retain(|port| port.name != "pose");
        }
        group.interface.outputs.retain(|port| port.name != "body");
        let output_still_used = group.wires.iter().any(|wire| wire.to_node == output_id);
        if !output_still_used {
            group.nodes.retain(|node| node.id != output_id);
        }
    }
    Ok(())
}

pub(super) fn strip_group_physics_for_split(group: &mut GroupDef) -> Result<(), &'static str> {
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
pub(super) struct ImportedPhysicsBinding {
    pub(super) world_id: u32,
    pub(super) body_slot: u32,
    pub(super) body_id: u32,
}

pub(super) fn scene_physics_binding(
    def: &EffectGraphDef,
    parts: &SceneObjectParts,
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

pub(super) fn remove_string_binding_target(def: &mut EffectGraphDef, node_id: &NodeId) {
    if let Some(meta) = def.preset_metadata.as_mut() {
        meta.string_bindings.retain(|binding| {
            !matches!(&binding.target, BindingTarget::Node { node_id: target, .. } if target == node_id)
        });
    }
}

#[derive(Debug, Clone)]
pub(super) struct ScenePhysicsPlan {
    parts: SceneObjectParts,
    body_params: BTreeMap<String, SerializedParamValue>,
    world_id: u32,
    body_slot: u32,
    world_exists: bool,
}

/// The shared structural preflight for the physics projection and command.
/// Keeping this on the editing side means a stale UI snapshot cannot make the
/// command accept a graph shape that the content thread would later reject.
/// Whether render slot `object_index` shows water: its surface walks to a
/// liquid domain (`manifold_core::liquid_domain::liquid_domain_of`, the walk
/// forces and the scene panel use). A graph the scene index refuses has no
/// stable paths, so nothing in it is water.
fn scene_object_is_water(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
) -> Result<bool, String> {
    use manifold_core::liquid_domain::liquid_domain_of;
    use manifold_core::scene_index::FlatSceneIndex;
    use manifold_core::SceneNodeRef;

    let Some(scene) = def.nodes.iter().find(|node| node.id == render_scene_node_id) else {
        return Ok(false);
    };
    let Ok(index) = FlatSceneIndex::build(def) else {
        return Ok(false);
    };
    let scene = SceneNodeRef { scope: Vec::new(), node: scene.node_id.clone() };
    let Ok(Some(object)) = index.scene_object_at(&scene, object_index) else {
        return Ok(false);
    };
    liquid_domain_of(&index, &object)
        .map(|domain| domain.is_some())
        .map_err(|error| format!("Enable Physics cannot read this object's surface: {error}"))
}

pub(super) fn scene_object_physics_plan(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
) -> Result<ScenePhysicsPlan, String> {
    if scene_object_is_water(def, render_scene_node_id, object_index)? {
        return Err(
            "Water cannot take Enable Physics: its liquid simulation already moves it".into(),
        );
    }
    let parts = scene_object_parts(def, render_scene_node_id, object_index)?;
    if parts.render_indices.is_empty() {
        return Err("Selected scene object has no render outputs".into());
    }
    if scene_physics_binding(def, &parts).is_ok() {
        return Err("Selected object already has standard physics enabled".into());
    }
    let has_fluid_role = match parts.group_id {
        Some(group_id) => !scene_fluid_role_assignments(def, group_id)?.is_empty(),
        None => fluid::loose_scene_object_has_fluid_roles(def, parts.object_id)?,
    };
    if has_fluid_role {
        return Err(
            "Remove this object's Fluid Role before enabling Physics; physics bodies interact with water automatically".into(),
        );
    }
    if parts
        .compound_sources
        .iter()
        .any(|source| source.kind != parts.source.kind)
    {
        return Err("Enable Physics cannot mix builtin and imported mesh sources".into());
    }
    if parts.source.kind == ScenePhysicsSourceKind::Builtin
        && parts.compound_sources.len() > 1
    {
        return Err("Enable Physics does not support compound builtin mesh groups".into());
    }
    let mut body_params = scene_body_params(&parts.source, def)?;
    if parts.source.kind == ScenePhysicsSourceKind::Imported
        && parts.compound_sources.len() > 1
    {
        body_params.insert(
            "compound_materials".to_string(),
            compound_materials_param(&parts.compound_sources)?,
        );
    }
    body_params.insert("motion".to_string(), SerializedParamValue::Enum { value: 1 });
    body_params.insert("mass".to_string(), SerializedParamValue::Float { value: 1.0 });
    body_params.insert("friction".to_string(), SerializedParamValue::Float { value: 0.5 });
    body_params.insert("bounce".to_string(), SerializedParamValue::Float { value: 0.15 });

    let worlds: Vec<u32> = def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.physics_world")
        .map(|node| node.id)
        .collect();
    if worlds.len() > 1 {
        return Err("Enable Physics requires one shared root Physics World".into());
    }
    let world_exists = !worlds.is_empty();
    let world_id = worlds.first().copied().unwrap_or(
        max_node_id_over(&def.nodes)
            .checked_add(1)
            .ok_or_else(|| "Enable Physics document id space is exhausted".to_string())?,
    );
    let body_slot = first_free_physics_body_slot(&def.wires, world_id)
        .ok_or_else(|| "Physics World has no free body slots".to_string())?;
    Ok(ScenePhysicsPlan {
        parts,
        body_params,
        world_id,
        body_slot,
        world_exists,
    })
}

/// Pure eligibility shared with the scene projection.  It deliberately runs
/// the same source, fluid-role, world, and body-slot checks as Enable.
pub fn scene_object_physics_eligibility(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
    object_index: u32,
) -> Result<(), String> {
    scene_object_physics_plan(def, render_scene_node_id, object_index).map(|_| ())
}

/// Enable standard physics for one scene object. Grouped imports expose a
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
        let plan = match scene_object_physics_plan(
            def,
            self.render_scene_node_id,
            self.object_index,
        ) {
            Ok(plan) => plan,
            Err(reason) => {
                self.rejection = Some(reason);
                return;
            }
        };
        let ScenePhysicsPlan {
            parts,
            body_params,
            world_id,
            body_slot,
            world_exists,
        } = plan;
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
            if !world_exists {
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
                    (parts.source.kind == ScenePhysicsSourceKind::Builtin)
                        .then_some(parts.source.source_id),
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
                if parts.source.kind == ScenePhysicsSourceKind::Builtin {
                    def.wires.push(scene_build_wire(
                        parts.source.source_id,
                        "source",
                        body_id,
                        "source",
                    ));
                }
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
            if !world_exists
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
                Some("Physics edit requires an unmodified scene object graph".into());
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

/// Remove the body/world wiring while leaving the scene visual object and
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
        let Ok(parts) = scene_object_parts(def, self.render_scene_node_id, self.object_index)
        else {
            self.rejection =
                Some("Disable Physics supports standard scene object mesh sources only".into());
            return;
        };
        let Ok(binding) = scene_physics_binding(def, &parts) else {
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
                        || wire.from_node == binding.body_id
                        || wire.to_node == binding.body_id)
                });
                def.wires.push(scene_build_wire(
                    parts.authored_transform_id,
                    "transform",
                    parts.object_id,
                    "transform",
                ));
                def.nodes.retain(|node| node.id != binding.body_id);
            }
            // A removed body must not leave a force attached to a reusable slot.
            def.wires.retain(|wire| {
                !(wire.to_node == binding.world_id
                    && wire.to_port == format!("body_acceleration_{}", binding.body_slot))
            });
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
                Some("Physics edit requires an unmodified scene object graph".into());
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
