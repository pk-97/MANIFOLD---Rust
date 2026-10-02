//! Load-time migration for legacy FLIP obstacle scene objects.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GroupDef, SerializedParamValue,
};
use manifold_core::group_edit::group_selection;
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, is_liquid_domain};
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::scene_object_migration::loose_scene_object_owned_ids;
use manifold_core::{NodeId, short_id};

const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const TRANSFORM_TYPE_ID: &str = "node.transform_3d";
const CUBE_MESH_TYPE_ID: &str = "node.cube_mesh";
const PLATONIC_MESH_TYPE_ID: &str = "node.platonic_solid_mesh";
const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

/// Migrate recognized root-level legacy obstacle objects into the standard
/// grouped Collider role shape. The caller wires this into the existing load
/// migration chain; this function deliberately leaves malformed or shared
/// shapes unchanged.
pub(super) fn migrate(def: &mut EffectGraphDef) -> bool {
    if !def.nodes.iter().any(|node| is_liquid_domain(&node.type_id)) {
        return false;
    }
    let mut candidate = def.clone();
    let mut changed = false;
    let mut index = 0;
    while index < candidate.nodes.len() {
        let Some((fluid_id, object_id, authored_transform_id)) =
            find_legacy_obstacle(&candidate, index)
        else {
            index += 1;
            continue;
        };
        if migrate_one(&mut candidate, fluid_id, object_id, authored_transform_id) {
            changed = true;
            index = 0;
        } else {
            index += 1;
        }
    }
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

fn find_legacy_obstacle(def: &EffectGraphDef, start: usize) -> Option<(u32, u32, u32)> {
    for fluid in def.nodes.iter().skip(start) {
        if fluid.type_id != FLIP_DOMAIN_TYPE_ID {
            continue;
        }
        let legacy_inputs: Vec<_> =
            def.wires
                .iter()
                .filter(|wire| {
                    wire.from_node == fluid.id
                        && wire.from_port == "obstacle_pose"
                        && wire.to_port == "transform"
                        && def.nodes.iter().any(|node| {
                            node.id == wire.to_node && node.type_id == SCENE_OBJECT_TYPE_ID
                        })
                })
                .collect();
        if legacy_inputs.len() != 1 {
            continue;
        }
        let authored_outputs: Vec<_> =
            def.wires
                .iter()
                .filter(|wire| {
                    wire.to_node == fluid.id
                        && wire.to_port == "obstacle"
                        && def.nodes.iter().any(|node| {
                            node.id == wire.from_node && node.type_id == TRANSFORM_TYPE_ID
                        })
                })
                .collect();
        if authored_outputs.len() != 1 {
            continue;
        }
        return Some((
            fluid.id,
            legacy_inputs[0].to_node,
            authored_outputs[0].from_node,
        ));
    }
    None
}

fn migrate_one(
    def: &mut EffectGraphDef,
    fluid_id: u32,
    object_id: u32,
    authored_transform_id: u32,
) -> bool {
    let Some(object) = def.nodes.iter().find(|node| node.id == object_id) else {
        return false;
    };
    let Some(object_handle) = object.handle.clone() else {
        return false;
    };
    let Some(render_wire) = def.wires.iter().find(|wire| {
        wire.from_node == object_id
            && wire.from_port == "object"
            && def
                .nodes
                .iter()
                .any(|node| node.id == wire.to_node && node.type_id == RENDER_SCENE_TYPE_ID)
    }) else {
        return false;
    };
    let render_id = render_wire.to_node;
    let Some(mesh_wire) = def
        .wires
        .iter()
        .find(|wire| wire.to_node == object_id && wire.to_port == "vertices")
    else {
        return false;
    };
    let Some(mesh) = def.nodes.iter().find(|node| node.id == mesh_wire.from_node) else {
        return false;
    };
    if !matches!(
        mesh.type_id.as_str(),
        CUBE_MESH_TYPE_ID | PLATONIC_MESH_TYPE_ID
    ) {
        return false;
    }
    if def
        .wires
        .iter()
        .filter(|wire| wire.to_node == object_id && wire.to_port == "transform")
        .count()
        != 1
    {
        return false;
    }

    let Some(legacy_input) = def.wires.iter().position(|wire| {
        wire.from_node == fluid_id
            && wire.from_port == "obstacle_pose"
            && wire.to_node == object_id
            && wire.to_port == "transform"
    }) else {
        return false;
    };
    let Some(authored_output) = def.wires.iter().position(|wire| {
        wire.from_node == authored_transform_id
            && wire.to_node == fluid_id
            && wire.to_port == "obstacle"
    }) else {
        return false;
    };

    let old_stable_ids = all_stable_ids(&def.nodes);
    let mut candidate_wires = def.wires.clone();
    let authored_output_wire = candidate_wires[authored_output].clone();
    candidate_wires.remove(legacy_input.max(authored_output));
    candidate_wires.remove(legacy_input.min(authored_output));
    if authored_output_wire.from_node != authored_transform_id {
        return false;
    }
    if candidate_wires
        .iter()
        .any(|wire| wire.to_node == object_id && wire.to_port == "transform")
    {
        return false;
    }
    candidate_wires.push(EffectGraphWire {
        from_node: authored_transform_id,
        from_port: "transform".into(),
        to_node: object_id,
        to_port: "transform".into(),
    });

    let mut candidate_nodes = def.nodes.clone();
    let owned = loose_scene_object_owned_ids(&candidate_nodes, &candidate_wires, object_id);
    if !owned.contains(&authored_transform_id) || !owned.contains(&mesh_wire.from_node) {
        return false;
    }
    if owned.iter().any(|id| *id == fluid_id || *id == render_id) {
        return false;
    }
    let Some(role_id) = max_node_id_recursive(&candidate_nodes).checked_add(1) else {
        return false;
    };
    let role_node_id = unique_node_id(&format!("fluid_role_source_{role_id}"), &old_stable_ids);
    let role_node = role_node(role_id, role_node_id.clone());
    candidate_nodes.push(role_node);
    let role_port = first_free_role_port(&candidate_wires, fluid_id);
    let Some(role_port) = role_port else {
        return false;
    };
    candidate_wires.push(EffectGraphWire {
        from_node: role_id,
        from_port: "role".into(),
        to_node: fluid_id,
        to_port: role_port.clone(),
    });
    candidate_wires.push(EffectGraphWire {
        from_node: mesh_wire.from_node,
        from_port: "source".into(),
        to_node: role_id,
        to_port: "mesh_0".into(),
    });
    candidate_wires.push(EffectGraphWire {
        from_node: authored_transform_id,
        from_port: "transform".into(),
        to_node: role_id,
        to_port: "transform".into(),
    });

    let selected: BTreeSet<u32> = owned.into_iter().chain([role_id]).collect();
    let mut generated_stable_ids = old_stable_ids;
    generated_stable_ids.insert(role_node_id);
    group_object_with_role(
        def,
        candidate_nodes,
        candidate_wires,
        &selected,
        GroupedRole { fluid_id, render_id, role_id },
        &object_handle,
        &generated_stable_ids,
    )
}

/// A loose root object whose collider is a root role source sharing the
/// object's transform. Remove Object only disconnects roles that live inside
/// the object's group, so this shape left the collider wired to the solver
/// after the object was deleted. Grouping it gives the object ownership.
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
        let fluid_ok = out.to_port.starts_with("role_")
            && def.nodes.iter().any(|node| {
                node.id == out.to_node && is_liquid_domain(&node.type_id)
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

fn role_node(id: u32, node_id: NodeId) -> EffectGraphNode {
    let mut params = BTreeMap::new();
    params.insert("role".into(), SerializedParamValue::Enum { value: 3 });
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
    EffectGraphNode {
        id,
        node_id,
        type_id: ROLE_SOURCE_TYPE_ID.into(),
        handle: Some(format!("fluid_role_source_{id}")),
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}

fn int(value: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value }
}

fn first_free_role_port(wires: &[EffectGraphWire], fluid_id: u32) -> Option<String> {
    (0..64).map(|index| format!("role_{index}")).find(|port| {
        !wires
            .iter()
            .any(|wire| wire.to_node == fluid_id && wire.to_port == *port)
    })
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

fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| {
            node.id.max(
                node.group
                    .as_deref()
                    .map(|group| max_node_id_recursive(&group.nodes))
                    .unwrap_or(0),
            )
        })
        .max()
        .unwrap_or(0)
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

fn unique_node_id(prefix: &str, used: &HashSet<NodeId>) -> NodeId {
    let mut candidate = NodeId::new(prefix);
    let mut suffix = 2;
    while used.contains(&candidate) {
        candidate = NodeId::new(format!("{prefix}_{suffix}"));
        suffix += 1;
    }
    candidate
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use super::*;
    use crate::node_graph::scene_vm::{SceneObjectVm, SceneVm};
    use manifold_core::flatten::flatten_groups;
    use manifold_core::effect_graph_def::GROUP_TYPE_ID;

    const WATER_BASIN_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterBasin.json");
    const WATER_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreak.json");

    fn stable_ids(nodes: &[EffectGraphNode], out: &mut HashSet<NodeId>) {
        for node in nodes {
            out.insert(node.node_id.clone());
            if let Some(group) = node.group.as_deref() {
                stable_ids(&group.nodes, out);
            }
        }
    }

    fn assert_water_migration(label: &str, json: &str) {
        let mut def: EffectGraphDef = serde_json::from_str(json).expect("water preset parses");
        let mut original_stable_ids = HashSet::new();
        stable_ids(&def.nodes, &mut original_stable_ids);
        let original_binding_ids: BTreeSet<_> = def
            .preset_metadata
            .as_ref()
            .expect("water preset metadata")
            .bindings
            .iter()
            .map(|binding| binding.id.clone())
            .collect();

        assert!(
            migrate(&mut def),
            "{label} must migrate its legacy obstacle"
        );
        assert!(!migrate(&mut def), "{label} migration must be idempotent");

        let mut migrated_stable_ids = HashSet::new();
        stable_ids(&def.nodes, &mut migrated_stable_ids);
        assert!(
            original_stable_ids.is_subset(&migrated_stable_ids),
            "{label} migration must preserve every authored stable node id"
        );
        let migrated_binding_ids: BTreeSet<_> = def
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .map(|binding| binding.id.clone())
            .collect();
        assert_eq!(
            migrated_binding_ids, original_binding_ids,
            "{label} bindings changed"
        );
        assert!(!def.wires.iter().any(|wire| {
            wire.from_port == "obstacle_pose"
                || wire.to_port == "obstacle_pose"
                || wire.from_port == "obstacle"
                || wire.to_port == "obstacle"
        }));

        let group = def
            .nodes
            .iter()
            .find(|node| {
                node.type_id == GROUP_TYPE_ID && node.handle.as_deref() == Some("Moving Box")
            })
            .expect("migrated Moving Box group");
        let body = group.group.as_deref().expect("migrated group body");
        let role = body
            .nodes
            .iter()
            .find(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
            .expect("standard Collider role source");
        assert_eq!(
            role.params.get("role"),
            Some(&SerializedParamValue::Enum { value: 3 })
        );
        assert_eq!(
            role.params.get("geometry"),
            Some(&SerializedParamValue::Enum { value: 1 })
        );
        let role_output = format!("fluid_role_source_{}", role.id);
        assert!(
            body.interface
                .outputs
                .iter()
                .any(|port| { port.name == role_output && port.port_type == "FluidRole" })
        );
        assert!(
            body.interface
                .outputs
                .iter()
                .any(|port| { port.name == "object" && port.port_type == "Object" })
        );
        assert!(def.wires.iter().any(|wire| {
            wire.from_node == group.id
                && wire.from_port == role_output
                && wire.to_port.starts_with("role_")
        }));

        let vm = SceneVm::from_def(&def).expect("migrated scene remains discoverable");
        assert!(vm.objects.iter().any(|object| {
            matches!(object, SceneObjectVm::Known(row)
                if row.name == "Moving Box"
                    && row.transform.is_some()
                    && row.fluid_controls.contains(&role.node_id))
        }));
        flatten_groups(&def).unwrap_or_else(|error| panic!("migrated {label} graph must flatten: {error:?}"));

        let saved = serde_json::to_string(&def).expect("migrated graph serializes");
        let mut reloaded: EffectGraphDef =
            serde_json::from_str(&saved).expect("migrated graph reloads");
        assert_eq!(
            reloaded, def,
            "{label} save/reload must preserve the migration"
        );
        assert!(
            !migrate(&mut reloaded),
            "{label} reload must remain migrated"
        );
        assert_eq!(
            reloaded, def,
            "{label} reload migration must be byte-stable"
        );
    }

    #[test]
    fn water_basin_legacy_obstacle_migrates_to_grouped_collider() {
        assert_water_migration("WaterBasin", WATER_BASIN_JSON);
    }

    #[test]
    fn water_dam_break_legacy_obstacle_migrates_to_grouped_collider() {
        assert_water_migration("WaterDamBreak", WATER_DAM_BREAK_JSON);
    }

    const GPU_FLIP_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreakGpuFlip.json");
    const MATTER_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreakMatter.json");

    /// Role wires into any liquid domain, at any group depth.
    fn domain_role_inputs(def: &EffectGraphDef) -> usize {
        fn count(nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> usize {
            let here = wires
                .iter()
                .filter(|wire| {
                    wire.to_port.starts_with("role_")
                        && nodes.iter().any(|node| {
                            node.id == wire.to_node && is_liquid_domain(&node.type_id)
                        })
                })
                .count();
            here + nodes
                .iter()
                .filter_map(|node| node.group.as_deref())
                .map(|group| count(&group.nodes, &group.wires))
                .sum::<usize>()
        }
        count(&def.nodes, &def.wires)
    }

    fn project_with(def: &EffectGraphDef, preset: &'static str) -> (
        manifold_core::project::Project,
        manifold_core::GraphTarget,
    ) {
        use manifold_core::layer::Layer;
        use manifold_core::{GraphTarget, LayerId, PresetTypeId};

        let mut layer = Layer::new_generator("Dam".into(), PresetTypeId::new(preset), 0);
        let layer_id = LayerId::new("loose-obstacle-layer");
        layer.layer_id = layer_id.clone();
        let host = layer.gen_params_or_init();
        host.graph = Some(def.clone());
        host.refresh_manifest_from_graph();
        let mut project = manifold_core::project::Project::default();
        project.timeline.layers.push(layer);
        (project, GraphTarget::Generator(layer_id))
    }

    fn obstacle_slot(def: &EffectGraphDef) -> (u32, u32) {
        let render_id = def
            .nodes
            .iter()
            .find(|node| node.type_id == RENDER_SCENE_TYPE_ID)
            .expect("render scene")
            .id;
        let index = def
            .wires
            .iter()
            .find(|wire| {
                wire.to_node == render_id
                    && def.nodes.iter().any(|node| {
                        node.id == wire.from_node
                            && (node.handle.as_deref() == Some("Obstacle")
                                || node.node_id.as_str() == "obstacle_object")
                    })
            })
            .and_then(|wire| wire.to_port.strip_prefix("object_")?.parse().ok())
            .expect("Obstacle object slot");
        (render_id, index)
    }

    /// Remove Object on the obstacle through the real command, the way the
    /// Scene panel's delete issues it.
    fn delete_obstacle(def: EffectGraphDef, preset: &'static str) -> EffectGraphDef {
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::RemoveSceneObjectCommand;

        let (render_id, index) = obstacle_slot(&def);
        let (mut project, target) = project_with(&def, preset);
        let mut remove = RemoveSceneObjectCommand::new(target.clone(), vec![], render_id, index, def);
        remove.execute(&mut project);
        assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
        project
            .graph_target_owner(&target)
            .and_then(|owner| owner.graph.clone())
            .expect("edited graph")
    }

    fn assert_obstacle_leaves_solver(def: EffectGraphDef, preset: &'static str) {
        assert_eq!(domain_role_inputs(&def), 1, "{preset}: the obstacle is the only body");
        let after = delete_obstacle(def, preset);
        assert_eq!(domain_role_inputs(&after), 0, "{preset}: deleted obstacle still in the solver");
        assert!(
            !after.nodes.iter().any(|node| {
                node.handle.as_deref() == Some("Obstacle")
                    || matches!(
                        node.node_id.as_str(),
                        "obstacle_collider" | "obstacle_transform" | "obstacle_object"
                    )
            }),
            "{preset}: the obstacle's nodes must go with it"
        );
        flatten_groups(&after).expect("edited graph flattens");
    }

    #[test]
    fn gpu_flip_dam_break_obstacle_groups_with_its_collider() {
        let mut def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        assert!(migrate(&mut def), "loose collider must group with its object");
        assert!(!migrate(&mut def), "migration must be idempotent");
        assert!(
            !def.nodes.iter().any(|node| node.type_id == ROLE_SOURCE_TYPE_ID),
            "the collider must live inside the Obstacle group"
        );
        flatten_groups(&def).expect("migrated graph flattens");
        let vm = SceneVm::from_def(&def).expect("scene discoverable");
        assert!(vm.objects.iter().any(|object| matches!(object,
            SceneObjectVm::Known(row) if row.name == "Obstacle" && row.group_node_id.is_some())));
    }

    #[test]
    fn deleting_grouped_gpu_flip_obstacle_removes_its_collider() {
        let mut def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        migrate(&mut def);
        assert_obstacle_leaves_solver(def, "WaterDamBreakGpuFlip");
    }

    /// The command owns the rule, not the migration: a loose object's
    /// collider goes with it.
    #[test]
    fn deleting_loose_gpu_flip_obstacle_removes_its_collider() {
        let def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        assert_obstacle_leaves_solver(def, "WaterDamBreakGpuFlip");
    }

    #[test]
    fn deleting_matter_dam_break_obstacle_removes_its_collider() {
        let def: EffectGraphDef =
            serde_json::from_str(MATTER_DAM_BREAK_JSON).expect("preset parses");
        assert_obstacle_leaves_solver(def, "WaterDamBreakMatter");
    }

    #[test]
    fn enabling_physics_on_loose_obstacle_with_a_collider_is_refused() {
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand;

        let def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        let (render_id, index) = obstacle_slot(&def);
        let (mut project, target) = project_with(&def, "WaterDamBreakGpuFlip");
        let mut enable =
            EnableSceneObjectPhysicsCommand::new(target, render_id, index, Vec::new(), def);
        enable.execute(&mut project);
        assert!(!enable.was_applied());
        let reason = enable.rejection_reason().unwrap_or_default();
        assert!(reason.contains("Fluid Role"), "{reason}");
    }

    /// A loose collider that cannot group must not stop the others.
    #[test]
    fn ungroupable_loose_collider_does_not_block_the_rest() {
        let mut def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        let mut blocked = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_object")
            .unwrap()
            .clone();
        let ids = max_node_id_recursive(&def.nodes);
        let mut collider = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_collider")
            .unwrap()
            .clone();
        let transform = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_transform")
            .unwrap()
            .clone();
        // A handle-less object cannot be grouped; it is listed first.
        blocked.id = ids + 1;
        blocked.node_id = NodeId::new("blocked_object");
        blocked.handle = None;
        collider.id = ids + 2;
        collider.node_id = NodeId::new("blocked_collider");
        let mut blocked_transform = transform.clone();
        blocked_transform.id = ids + 3;
        blocked_transform.node_id = NodeId::new("blocked_transform");
        let domain = def
            .nodes
            .iter()
            .find(|node| node.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
            .unwrap()
            .id;
        let render = def
            .nodes
            .iter()
            .find(|node| node.type_id == RENDER_SCENE_TYPE_ID)
            .unwrap()
            .id;
        for (from_node, from_port, to_node, to_port) in [
            (ids + 3, "transform", ids + 1, "transform"),
            (ids + 3, "transform", ids + 2, "transform"),
            (ids + 2, "role", domain, "role_1"),
            (ids + 1, "object", render, "object_10"),
        ] {
            def.wires.push(EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
        }
        def.nodes.insert(0, blocked);
        def.nodes.insert(0, collider);
        def.nodes.push(blocked_transform);
        assert!(migrate(&mut def));
        let root_roles: Vec<_> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
            .map(|node| node.node_id.as_str())
            .collect();
        assert_eq!(root_roles, ["blocked_collider"], "the groupable obstacle must still group");
    }
}
