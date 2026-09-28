//! Physics scene command helpers extracted from `scene.rs`.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct PhysicsSceneObject {
    pub(super) world_id: u32,
    pub(super) body_slot: u32,
    pub(super) copies: bool,
    pub(super) transform_id: u32,
    pub(super) object_id: u32,
    pub(super) owned_ids: Vec<u32>,
    pub(super) grouped: bool,
    /// Every render slot fed by this producer, including compound material
    /// outputs from one imported group.
    pub(super) render_indices: Vec<u32>,
}

#[derive(Debug, Clone)]
pub(super) enum PhysicsSceneObjectMatch {
    NotPhysics,
    Valid(PhysicsSceneObject),
    Malformed(&'static str),
}

pub(super) fn unique_input<'a>(
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

pub(super) fn unique_output<'a>(
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

pub(super) fn physics_scene_object_match(
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

pub(super) fn grouped_physics_scene_object_match(
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

pub(super) fn group_render_indices(
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
pub(super) fn physics_copies_candidate(
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

pub(super) fn rigid_body_mesh_candidate(
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

pub(super) fn physics_copies_scene_object_match(
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

pub(super) fn first_free_physics_body_slot(wires: &[EffectGraphWire], world_id: u32) -> Option<u32> {
    (0..PHYSICS_BODY_SLOTS).find(|slot| physics_body_slot_available(wires, world_id, *slot))
}

pub(super) fn physics_body_slot_available(wires: &[EffectGraphWire], world_id: u32, slot: u32) -> bool {
    let body_port = format!("body_{slot}");
    let pose_port = format!("pose_{slot}");
    let field_port = format!("body_acceleration_{slot}");
    !wires.iter().any(|wire| {
        (wire.to_node == world_id && (wire.to_port == body_port || wire.to_port == field_port))
            || (wire.from_node == world_id && wire.from_port == pose_port)
    })
}
