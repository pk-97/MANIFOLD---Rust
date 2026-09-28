//! Pure route edits used by whole-object commands. Callers own a candidate
//! graph and install it only after every geometry, capacity and route check.

use super::*;

pub(in crate::commands::graph::scene) fn disconnect_scene_object_fluid_roles(
    def: &mut EffectGraphDef,
    group_doc_id: u32,
) -> Result<(), String> {
    let routes = scene_fluid_role_assignments(def, group_doc_id)?
        .iter()
        .map(|assignment| discover_role_route(def, &assignment.source))
        .collect::<Result<Vec<_>, _>>()?;
    for route in routes {
        remove_route_edges(def, &route, false)?;
    }
    Ok(())
}

/// Restore captured outgoing routes to their original domains atomically.
pub fn restore_scene_object_fluid_roles(
    def: &mut EffectGraphDef,
    assignments: &[SceneFluidRoleAssignment],
    cloned_group: u32,
    node_id_map: &[(NodeId, NodeId)],
) -> Result<(), String> {
    let mut candidate = def.clone();
    restore_scene_object_fluid_roles_in_place(
        &mut candidate,
        assignments,
        cloned_group,
        node_id_map,
    )?;
    *def = candidate;
    Ok(())
}

fn restore_scene_object_fluid_roles_in_place(
    def: &mut EffectGraphDef,
    assignments: &[SceneFluidRoleAssignment],
    cloned_group: u32,
    node_id_map: &[(NodeId, NodeId)],
) -> Result<(), String> {
    if assignments.is_empty() {
        return Ok(());
    }
    let cloned_scope = def
        .nodes
        .iter()
        .find(|node| node.id == cloned_group && node.type_id == GROUP_TYPE_ID)
        .ok_or_else(|| "Duplicated fluid role group is unavailable".to_string())?
        .node_id
        .clone();
    for assignment in assignments {
        let cloned_node = node_id_map
            .iter()
            .find(|(old, _)| old == &assignment.source.node)
            .ok_or_else(|| "Duplicated fluid role identity is unavailable".to_string())?
            .1
            .clone();
        let source = SceneNodeRef {
            scope: vec![cloned_scope.clone()],
            node: cloned_node,
        };
        let route = discover_role_route(def, &source)?;
        if !route.edges.is_empty() {
            return Err("Duplicated fluid role already has outward connections".into());
        }
        if assignment.domains.is_empty() {
            continue;
        }
        // Give each copied destination an independent export. This also avoids
        // sharing intermediate input boundaries between the original and copy.
        collapse_export_ports(def, cloned_group, route.role_id, "", &route.export_ports)?;
        for domain in &assignment.domains {
            let (scope, fluid_id) = resolve_domain_ref(def, domain)?;
            if scope.first() == Some(&cloned_group) {
                return Err(
                    "A fluid role cannot target a fluid inside its own source group".into(),
                );
            }
            let old_port = create_export_port(def, cloned_group, route.role_id)?;
            let source_port =
                collision_free_export_port(def, &scope, cloned_group, route.role_id, &old_port)?;
            if source_port != old_port {
                rename_export_port(def, cloned_group, route.role_id, &old_port, &source_port)?;
            }
            let target_port = first_free_role_port(def, &scope, fluid_id)?;
            let mut next_id = super::super::super::max_node_id_over(&def.nodes)
                .checked_add(1)
                .ok_or_else(|| "Fluid role document id space is exhausted".to_string())?;
            let mut fresh_id = || {
                let id = next_id;
                next_id = next_id.checked_add(1)?;
                Some(id)
            };
            route_role_to_domain(
                &mut def.nodes,
                &mut def.wires,
                &scope,
                cloned_group,
                source_port,
                target_port,
                fluid_id,
                &mut fresh_id,
            )?;
        }
    }
    Ok(())
}

pub(in crate::commands::graph::scene) fn duplicate_scene_object_fluid_roles(
    def: &mut EffectGraphDef,
    original_group: u32,
    cloned_group: u32,
    node_id_map: &[(NodeId, NodeId)],
) -> Result<(), String> {
    let assignments = scene_fluid_role_assignments(def, original_group)?;
    restore_scene_object_fluid_roles(def, &assignments, cloned_group, node_id_map)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{body_mut, role_graph};
    use super::*;

    #[test]
    fn scene_physics_object_fluid_routes_copy_fanout_and_disconnect_independently() {
        let mut graph = role_graph(true, true);
        let domain = body_mut(&mut graph, 30);
        let mut additional = domain
            .nodes
            .iter()
            .find(|node| node.id == 31)
            .unwrap()
            .clone();
        additional.id = 33;
        additional.node_id = NodeId::new("second_nested_fluid");
        domain.nodes.push(additional);
        domain
            .wires
            .push(scene_build_wire(60, "fluid_role_source_50", 33, "role_0"));
        let original = scene_fluid_role_assignments(&graph, 10).unwrap();
        assert_eq!(original[0].domains.len(), 3);
        let group = graph.nodes.iter().find(|node| node.id == 10).unwrap();
        let mut ids = Vec::new();
        let cloned = crate::commands::graph::scene::deep_clone_with_fresh_ids(
            group,
            &mut 100,
            &mut HashSet::new(),
            &mut ids,
        );
        let cloned_group = cloned.id;
        graph.nodes.push(cloned);
        graph
            .wires
            .push(scene_build_wire(cloned_group, "object", 0, "object_1"));
        duplicate_scene_object_fluid_roles(&mut graph, 10, cloned_group, &ids).unwrap();
        let copied = scene_fluid_role_assignments(&graph, cloned_group).unwrap();
        assert_eq!(copied.len(), 1);
        assert_ne!(copied[0].source, original[0].source);
        assert_eq!(copied[0].domains, original[0].domains);
        assert_eq!(scene_fluid_role_assignments(&graph, 10).unwrap(), original);
        assert!(manifold_core::flatten::flatten_groups(&graph).is_ok());
        disconnect_scene_object_fluid_roles(&mut graph, 10).unwrap();
        assert_eq!(
            scene_fluid_role_assignments(&graph, cloned_group).unwrap(),
            copied
        );
        assert!(
            scene_fluid_role_assignments(&graph, 10).unwrap()[0]
                .domains
                .is_empty()
        );
        let domain = body_mut(&mut graph, 30);
        assert_eq!(domain.interface.inputs.len(), 2);
        assert!(
            domain
                .interface
                .inputs
                .iter()
                .all(|port| port.name != "fluid_role_source_50")
        );
        assert!(manifold_core::flatten::flatten_groups(&graph).is_ok());
    }

    #[test]
    fn captured_fluid_roles_restore_after_original_group_is_removed() {
        let mut graph = role_graph(true, true);
        let assignments = scene_fluid_role_assignments(&graph, 10).unwrap();
        let group = graph.nodes.iter().find(|node| node.id == 10).unwrap();
        let mut ids = Vec::new();
        let cloned = crate::commands::graph::scene::deep_clone_with_fresh_ids(
            group,
            &mut 100,
            &mut HashSet::new(),
            &mut ids,
        );
        let cloned_group = cloned.id;
        graph.nodes.push(cloned);
        disconnect_scene_object_fluid_roles(&mut graph, 10).unwrap();
        graph.nodes.retain(|node| node.id != 10);
        graph
            .wires
            .retain(|wire| wire.from_node != 10 && wire.to_node != 10);
        graph
            .wires
            .push(scene_build_wire(cloned_group, "object", 0, "object_0"));
        restore_scene_object_fluid_roles(&mut graph, &assignments, cloned_group, &ids).unwrap();
        let copied = scene_fluid_role_assignments(&graph, cloned_group).unwrap();
        assert_eq!(copied[0].domains, assignments[0].domains);
        assert!(manifold_core::flatten::flatten_groups(&graph).is_ok());
    }

    #[test]
    fn captured_fluid_roles_reject_missing_destination_atomically() {
        let mut graph = role_graph(true, true);
        let mut assignments = scene_fluid_role_assignments(&graph, 10).unwrap();
        assert_eq!(assignments[0].domains.len(), 2);
        // The first route succeeds; the second must roll back the entire edit.
        assignments[0].domains[1].node = NodeId::new("missing_destination");
        let group = graph.nodes.iter().find(|node| node.id == 10).unwrap();
        let mut ids = Vec::new();
        let cloned = crate::commands::graph::scene::deep_clone_with_fresh_ids(
            group,
            &mut 100,
            &mut HashSet::new(),
            &mut ids,
        );
        let cloned_group = cloned.id;
        graph.nodes.push(cloned);
        let before = graph.clone();
        let error =
            restore_scene_object_fluid_roles(&mut graph, &assignments, cloned_group, &ids).unwrap_err();
        assert!(error.contains("domain"), "{error}");
        assert_eq!(graph, before);
    }
}
