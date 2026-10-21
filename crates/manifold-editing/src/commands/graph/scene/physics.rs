//! Physics scene command helpers extracted from `scene.rs`.

use super::*;
use manifold_core::change::ChangeSet;
use manifold_core::scene_graph_edit::{SceneGraphEdit, SceneGraphEditError};

use crate::scene_transaction::SceneGraphTransaction;
use manifold_core::physics_members::Family;

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
    if node.type_id != "node.rigid_body" || wire.from_port != "pose" {
        return Err("Enable Physics supports a direct authored transform only");
    }
    let source = wires
        .iter()
        .find(|w| w.to_node == node.id && w.to_port == "transform")
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
    if node.type_id == "node.rigid_body" && wire.from_port == "pose" {
        let body_transform = group
            .wires
            .iter()
            .find(|candidate| candidate.to_node == node.id && candidate.to_port == "transform")
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
    if sources.len() > COMPOUND_MAX_PARTS as usize {
        static TOO_MANY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            format!("Physics compound objects support at most {COMPOUND_MAX_PARTS} material parts")
        });
        return Err(TOO_MANY.as_str());
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

/// The group's scene object reads its pose from exactly one wire, the body's
/// `pose` output, and from nowhere else.
pub(super) fn group_pose_source(group: &GroupDef, object_id: u32, body_id: u32) -> Result<(), &'static str> {
    let mut pose_wires = group.wires.iter().filter(|wire| {
        wire.to_node == object_id
            && (wire.to_port == "parent_transform" || wire.to_port == "transform")
            && wire.from_port == "pose"
    });
    let pose_wire = pose_wires.next().ok_or("Physics group pose is missing")?;
    if pose_wires.next().is_some() {
        return Err("Physics group pose is ambiguous");
    }
    if group
        .wires
        .iter()
        .filter(|wire| wire.to_node == object_id && wire.to_port == pose_wire.to_port)
        .count()
        != 1
    {
        return Err("Physics group pose is ambiguous");
    }
    if pose_wire.from_node != body_id {
        return Err("Physics group pose source has the wrong type");
    }
    Ok(())
}

/// Add a rigid body inside an import group. The group exposes the body's
/// membership as a `body` output, and its scene objects read `body.pose`
/// directly, so the Physics World stays outside the group and no pose input
/// crosses the boundary.
pub(super) fn add_group_physics(
    group: &mut GroupDef,
    body_id: u32,
    body_params: BTreeMap<String, SerializedParamValue>,
    body_handle: String,
    authored_transform_id: u32,
    object_id: u32,
    source_id: Option<u32>,
) -> Result<NodeId, &'static str> {
    let output_id = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|node| node.id)
        .ok_or("Physics group has no output boundary")?;
    if group.interface.outputs.iter().any(|p| p.name == "body") {
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
    group.nodes.push(body);
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
            wire.from_node = body_id;
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
    Ok(body_node_id)
}

/// Ok when the object already carries a well-formed standard physics body:
/// a rigid body wired into a body slot of a Physics World whose `pose`
/// drives the object.
pub(super) fn scene_physics_body_wired(def: &EffectGraphDef, parts: &SceneObjectParts) -> Result<(), &'static str> {
    use manifold_core::physics_members::{Family, world_member};
    let in_world = |body_id: u32, malformed: &'static str| -> Result<(), &'static str> {
        let member = world_member(&def.nodes, &def.wires, body_id).ok_or(malformed)?;
        let world = def
            .nodes
            .iter()
            .find(|node| node.id == member.world)
            .ok_or("Physics object world is unavailable")?;
        if member.family != Family::Body || member.k >= manifold_core::physics_body_slots::world_bodies(world) {
            return Err(malformed);
        }
        Ok(())
    };
    let Some(group_id) = parts.group_id else {
        let pose_wire = def
            .wires
            .iter()
            .find(|wire| wire.to_node == parts.object_id && wire.to_port == "transform")
            .ok_or("Physics object pose input is missing")?;
        let body = def
            .nodes
            .iter()
            .find(|node| node.id == pose_wire.from_node)
            .ok_or("Physics object body is unavailable")?;
        if body.type_id != "node.rigid_body" || pose_wire.from_port != "pose" {
            return Err("Selected object does not have standard physics enabled");
        }
        return in_world(body.id, "Physics object body input is missing");
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
    // The body output boundary is fed by the body node, so resolve that
    // direction explicitly rather than trusting a hand-authored port name.
    let body_id = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == "body")
        .map(|wire| wire.from_node)
        .ok_or("Physics object group body output is missing")?;
    let body = group
        .nodes
        .iter()
        .find(|node| node.id == body_id)
        .ok_or("Physics object group body is unavailable")?;
    if body.type_id != "node.rigid_body" {
        return Err("Physics object group body has the wrong type");
    }
    // The group's body output is the membership wire into the world.
    in_world(parts.producer_id, "Physics object group body slot is missing")
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
    if scene_physics_body_wired(def, &parts).is_ok() {
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
pub fn scene_object_physics_eligibility(def: &EffectGraphDef, slot: manifold_core::scene_slots::SceneSlot) -> Result<(), String> {
    scene_object_physics_plan(def, slot.render_id, slot.index).map(|_| ())
}

/// The graph with standard physics enabled on one scene object: a rigid body
/// in the next free slot of the scene's one shared Physics World (created
/// with the first body), its controls stamped from `body_metadata` and, for a
/// new world, `world_metadata`. Grouped imports expose a `body` output and
/// take a `pose` input so the world stays outside the object's group; the
/// flattener folds that boundary into the wiring a bare object has.
pub fn enable_physics_edit(
    graph: &EffectGraphDef,
    object: &SceneNodeRef,
    body_metadata: &[NodeParamMetadata],
    world_metadata: &[NodeParamMetadata],
) -> Result<SceneGraphEdit, SceneGraphEditError> {
    let refuse = |message: &str| SceneGraphEditError { at: None, message: message.into() };
    let slot = manifold_core::scene_slots::object_slot(graph, object).map_err(|message| refuse(&message))?;
    let ScenePhysicsPlan { parts, body_params, world_id, body_slot, world_exists } =
        scene_object_physics_plan(graph, slot.render_id, slot.index).map_err(|message| refuse(&message))?;
    let mut def = graph.clone();
    let mut next_id = max_node_id_over(&def.nodes)
        .checked_add(1)
        .ok_or_else(|| refuse("Enable Physics document id space is exhausted"))?;
    let mut taken = std::collections::HashSet::new();
    collect_all_handles(&def.nodes, &mut taken);
    if !world_exists {
        let handle = dedup_handle("Physics World", &mut taken);
        def.nodes.push(fresh_scene_node(next_id, "node.physics_world", Some(handle), BTreeMap::new()));
        next_id += 1;
    }
    let body_id = next_id;
    let body_handle = dedup_handle(&format!("{} Physics", parts.object_handle), &mut taken);
    let builtin_source =
        (parts.source.kind == ScenePhysicsSourceKind::Builtin).then_some(parts.source.source_id);
    let body_node_id = if let Some(group_id) = parts.group_id {
        let group = def
            .nodes
            .iter_mut()
            .find(|node| node.id == group_id)
            .and_then(|node| node.group.as_deref_mut())
            .ok_or_else(|| refuse("Enable Physics object group is unavailable"))?;
        let node_id = add_group_physics(
            group,
            body_id,
            body_params.clone(),
            body_handle,
            parts.authored_transform_id,
            parts.object_id,
            builtin_source,
        )
        .map_err(refuse)?;
        def.wires.push(scene_build_wire(group_id, "body", world_id, &Family::Body.member_port(body_slot)));
        node_id
    } else {
        let object_wire = def
            .wires
            .iter_mut()
            .find(|wire| wire.to_node == parts.object_id && wire.to_port == "transform")
            .ok_or_else(|| refuse("Enable Physics requires an authored transform"))?;
        object_wire.from_node = body_id;
        object_wire.from_port = "pose".into();
        let body = fresh_scene_node(body_id, "node.rigid_body", Some(body_handle), body_params.clone());
        let body_node_id = body.node_id.clone();
        def.nodes.push(body);
        def.wires.push(scene_build_wire(parts.authored_transform_id, "transform", body_id, "transform"));
        if let Some(source_id) = builtin_source {
            def.wires.push(scene_build_wire(source_id, "source", body_id, "source"));
        }
        def.wires.push(scene_build_wire(body_id, "body", world_id, &Family::Body.member_port(body_slot)));
        body_node_id
    };
    fit_world_bodies(&mut def.nodes, &def.wires, world_id);
    let string_binding = source_string_binding(&def, &parts.source, body_node_id.clone());
    let world = (!world_exists)
        .then(|| def.nodes.iter().find(|node| node.id == world_id).cloned())
        .flatten();
    let meta = def.preset_metadata.get_or_insert_with(unnamed_scene_metadata);
    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        body_id,
        &body_node_id,
        &format!("{} — Physics", parts.object_handle),
        body_metadata,
        &body_params,
    );
    if let Some(world) = world {
        stamp_scene_node_exposures_into(
            &mut meta.params,
            &mut meta.bindings,
            world.id,
            &world.node_id,
            "Physics World",
            world_metadata,
            &world.params,
        );
    }
    meta.string_bindings.extend(string_binding);
    Ok(SceneGraphEdit { graph: def, removed_param_ids: Vec::new(), parameter_id_remaps: Vec::new() })
}

/// Enable standard physics on one scene object ([`enable_physics_edit`]),
/// prepared when made so a refusal surfaces before submission and redo
/// reuses the same ids. Switching an existing body off and on writes its
/// `enabled` param instead; this command only creates the body.
#[derive(Debug)]
pub struct EnableSceneObjectPhysicsCommand {
    transaction: SceneGraphTransaction,
}

impl EnableSceneObjectPhysicsCommand {
    pub fn new(
        project: &Project,
        target: GraphTarget,
        object: SceneNodeRef,
        body_metadata: &[NodeParamMetadata],
        world_metadata: &[NodeParamMetadata],
        catalog_default: &EffectGraphDef,
    ) -> Result<Self, SceneGraphEditError> {
        Ok(Self {
            transaction: SceneGraphTransaction::prepare(project, target, catalog_default, "Enable Physics", |graph| {
                enable_physics_edit(graph, &object, body_metadata, world_metadata)
            })?,
        })
    }
}

impl Command for EnableSceneObjectPhysicsCommand {
    fn touches(&self) -> ChangeSet {
        self.transaction.touches()
    }
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.transaction.owner.clone());
    }
    fn do_execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn do_undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.transaction.rejection_reason()
    }
}

