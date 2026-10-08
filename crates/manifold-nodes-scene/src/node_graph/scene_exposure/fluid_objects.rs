//! Load-time migration for legacy FLIP obstacle scene objects.

use std::collections::{BTreeSet, HashSet};

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GroupDef,
};
use manifold_core::group_edit::group_selection;
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::scene_object_migration::loose_scene_object_owned_ids;
use manifold_core::{NodeId, short_id};

const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const TRANSFORM_TYPE_ID: &str = "node.transform_3d";
const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

/// Migrate recognized root-level legacy obstacle objects into the standard
/// grouped Collider role shape. The caller wires this into the existing load
/// migration chain; this function deliberately leaves malformed or shared
/// shapes unchanged.
pub(super) fn migrate(def: &mut EffectGraphDef) -> bool {
    let mut candidate = def.clone();
    let mut changed = false;
    // A shape that cannot group stays loose; the delete command still owns
    // its roles. Skip it and keep grouping the rest.
    let mut skipped = HashSet::new();
    while let Some((fluid_id, object_id, role_id)) =
        find_loose_role_obstacle(&candidate, &skipped)
    {
        if group_loose_role_obstacle(&mut candidate, fluid_id, object_id, role_id) {
            changed = true;
        } else {
            skipped.insert(role_id);
        }
    }
    if changed {
        *def = candidate;
    }
    changed
}

fn find_loose_role_obstacle(
    def: &EffectGraphDef,
    skipped: &HashSet<u32>,
) -> Option<(u32, u32, u32)> {
    for role in def
        .nodes
        .iter()
        .filter(|node| node.type_id == ROLE_SOURCE_TYPE_ID && !skipped.contains(&node.id))
    {
        let outs: Vec<_> = def.wires.iter().filter(|wire| wire.from_node == role.id).collect();
        let [out] = outs.as_slice() else { continue };
        // Trace the authored feed through at most one group boundary. Flattening
        // is unavailable while saved scene modifiers are still unexpanded.
        let fluid_ok = out.from_port == "role" && def.nodes.iter()
            .find(|node| node.id == out.to_node)
            .is_some_and(|target| {
                if is_liquid_domain(&target.type_id) {
                    return out.to_port.starts_with("role_");
                }
                target.group.as_ref().is_some_and(|group| {
                    group.wires.iter().any(|wire| {
                        wire.from_port == out.to_port
                            && wire.to_port.starts_with("role_")
                            && group.nodes.iter().any(|node|
                                node.id == wire.from_node && node.type_id == GROUP_INPUT_TYPE_ID)
                            && group.nodes.iter().any(|node|
                                node.id == wire.to_node && is_liquid_domain(&node.type_id))
                    })
                })
            });
        if !fluid_ok {
            continue;
        }
        let ins: Vec<_> = def.wires.iter().filter(|wire| wire.to_node == role.id).collect();
        let [input] = ins.as_slice() else { continue };
        let transform_id = input.from_node;
        if input.to_port != "transform"
            || !def
                .nodes
                .iter()
                .any(|node| node.id == transform_id && node.type_id == TRANSFORM_TYPE_ID)
        {
            continue;
        }
        let transform_outs: Vec<_> = def
            .wires
            .iter()
            .filter(|wire| wire.from_node == transform_id && wire.to_node != role.id)
            .collect();
        let [object_wire] = transform_outs.as_slice() else { continue };
        let is_object = object_wire.to_port == "transform"
            && def.nodes.iter().any(|node| {
                node.id == object_wire.to_node && node.type_id == SCENE_OBJECT_TYPE_ID
            });
        if is_object {
            return Some((out.to_node, object_wire.to_node, role.id));
        }
    }
    None
}

fn group_loose_role_obstacle(
    def: &mut EffectGraphDef,
    fluid_id: u32,
    object_id: u32,
    role_id: u32,
) -> bool {
    let Some(object_handle) = def
        .nodes
        .iter()
        .find(|node| node.id == object_id)
        .and_then(|node| node.handle.clone())
    else {
        return false;
    };
    let Some(render_id) = def
        .wires
        .iter()
        .find(|wire| {
            wire.from_node == object_id
                && wire.from_port == "object"
                && def
                    .nodes
                    .iter()
                    .any(|node| node.id == wire.to_node && node.type_id == RENDER_SCENE_TYPE_ID)
        })
        .map(|wire| wire.to_node)
    else {
        return false;
    };
    // Ownership is walked without the role's transform input, so the shared
    // transform counts as the object's own.
    let object_wires: Vec<_> = def
        .wires
        .iter()
        .filter(|wire| wire.to_node != role_id)
        .cloned()
        .collect();
    let owned = loose_scene_object_owned_ids(&def.nodes, &object_wires, object_id);
    if owned.iter().any(|id| *id == fluid_id || *id == render_id) {
        return false;
    }
    let selected: BTreeSet<u32> = owned.into_iter().chain([role_id]).collect();
    let stable_ids = all_stable_ids(&def.nodes);
    let (nodes, wires) = (def.nodes.clone(), def.wires.clone());
    group_object_with_role(
        def,
        nodes,
        wires,
        &selected,
        GroupedRole { fluid_id, render_id, role_id },
        &object_handle,
        &stable_ids,
    )
}

#[derive(Clone, Copy)]
struct GroupedRole {
    fluid_id: u32,
    render_id: u32,
    role_id: u32,
}

/// Group `selected` into one object group whose outputs are `object` (to the
/// render scene) and `fluid_role_source_{role_id}` (to the fluid domain).
fn group_object_with_role(
    def: &mut EffectGraphDef,
    candidate_nodes: Vec<EffectGraphNode>,
    candidate_wires: Vec<EffectGraphWire>,
    selected: &BTreeSet<u32>,
    ids: GroupedRole,
    object_handle: &str,
    generated_stable_ids: &HashSet<NodeId>,
) -> bool {
    let GroupedRole { fluid_id, render_id, role_id } = ids;
    let Some(group_id) = candidate_nodes
        .iter()
        .map(|node| node.id)
        .max()
        .and_then(|id| id.checked_add(1))
    else {
        return false;
    };
    let Some((mut nodes, mut wires)) = group_selection(
        candidate_nodes,
        candidate_wires,
        selected,
        object_handle,
        (0.0, 0.0),
    )
    .ok() else {
        return false;
    };
    let Some(group_index) = nodes.iter().position(|node| node.id == group_id) else {
        return false;
    };
    nodes[group_index].handle = Some(object_handle.to_string());
    let Some(group) = nodes[group_index].group.as_deref_mut() else {
        return false;
    };
    let Some(role_output) = wires
        .iter()
        .find(|wire| wire.from_node == group_id && wire.to_node == fluid_id)
        .map(|wire| wire.from_port.clone())
    else {
        return false;
    };
    let Some(object_output) = wires
        .iter()
        .find(|wire| wire.from_node == group_id && wire.to_node == render_id)
        .map(|wire| wire.from_port.clone())
    else {
        return false;
    };
    let role_output_name = format!("fluid_role_source_{role_id}");
    rename_group_output(group, &role_output, &role_output_name, "FluidRole");
    rename_group_output(group, &object_output, "object", "Object");
    for wire in wires.iter_mut().filter(|wire| wire.from_node == group_id) {
        if wire.from_port == role_output {
            wire.from_port = role_output_name.clone();
        } else if wire.from_port == object_output {
            wire.from_port = "object".into();
        }
    }
    repair_generated_stable_ids(&mut nodes[group_index], generated_stable_ids);
    update_scene_refs(def, &mut nodes[group_index]);
    def.nodes = nodes;
    def.wires = wires;
    true
}

fn rename_group_output(group: &mut GroupDef, old: &str, new: &str, port_type: &str) {
    if old == new {
        if let Some(port) = group
            .interface
            .outputs
            .iter_mut()
            .find(|port| port.name == old)
        {
            port.port_type = port_type.into();
        }
        return;
    }
    if let Some(port) = group
        .interface
        .outputs
        .iter_mut()
        .find(|port| port.name == old)
    {
        port.name = new.into();
        port.port_type = port_type.into();
    }
    let output_ids: HashSet<u32> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|node| node.id)
        .collect();
    for wire in &mut group.wires {
        if output_ids.contains(&wire.to_node) && wire.to_port == old {
            wire.to_port = new.into();
        }
    }
}


fn all_stable_ids(nodes: &[EffectGraphNode]) -> HashSet<NodeId> {
    fn visit(nodes: &[EffectGraphNode], ids: &mut HashSet<NodeId>) {
        for node in nodes {
            ids.insert(node.node_id.clone());
            if let Some(group) = node.group.as_deref() {
                visit(&group.nodes, ids);
            }
        }
    }
    let mut ids = HashSet::new();
    visit(nodes, &mut ids);
    ids
}

fn repair_generated_stable_ids(group_node: &mut EffectGraphNode, used: &HashSet<NodeId>) {
    let mut taken = used.clone();
    let group_id = group_node.id;
    group_node.node_id = fresh_generated_id(&format!("scene_object_group_{group_id}"), &mut taken);
    let Some(group) = group_node.group.as_deref_mut() else {
        return;
    };
    for node in &mut group.nodes {
        if matches!(
            node.type_id.as_str(),
            GROUP_INPUT_TYPE_ID | GROUP_OUTPUT_TYPE_ID
        ) {
            node.node_id = fresh_generated_id(
                &format!(
                    "scene_object_{}_{}",
                    node.type_id.replace('.', "_"),
                    node.id
                ),
                &mut taken,
            );
        }
    }
}

fn fresh_generated_id(prefix: &str, used: &mut HashSet<NodeId>) -> NodeId {
    let mut candidate = NodeId::new(format!("{prefix}_{}", short_id()));
    while !used.insert(candidate.clone()) {
        candidate = NodeId::new(format!("{prefix}_{}", short_id()));
    }
    candidate
}

fn update_scene_refs(def: &mut EffectGraphDef, group_node: &mut EffectGraphNode) {
    let group_stable = group_node.node_id.clone();
    let Some(group) = group_node.group.as_deref() else {
        return;
    };
    let moved: HashSet<NodeId> = group
        .nodes
        .iter()
        .filter(|node| {
            !matches!(
                node.type_id.as_str(),
                GROUP_INPUT_TYPE_ID | GROUP_OUTPUT_TYPE_ID
            )
        })
        .map(|node| node.node_id.clone())
        .collect();
    let update = |reference: &mut SceneNodeRef| {
        if moved.contains(&reference.node) && reference.scope.first() != Some(&group_stable) {
            reference.scope.insert(0, group_stable.clone());
        }
    };
    for modifier in &mut def.scene_modifiers {
        update(&mut modifier.scene);
        if let manifold_core::scene_modifier_preset::SceneTargetSelection::Explicit { objects } =
            &mut modifier.targets
        {
            for object in objects {
                update(object);
            }
        }
        for frame in &mut modifier.mesh_frames {
            update(&mut frame.target);
            update(&mut frame.source);
        }
    }
}
