//! Discovery and lifecycle commands for authored fluid roles.

mod object_routes;
pub(in crate::commands::graph::scene) use object_routes::{
    disconnect_scene_object_fluid_roles, duplicate_scene_object_fluid_roles,
};
pub use object_routes::restore_scene_object_fluid_roles;

use std::collections::HashSet;

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID,
};
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::SceneNodeRef;

use crate::command::Command;

use super::super::super::super::{
    InstanceLayerSnapshot, descend_level,
    refresh_target_manifest, scene_build_wire, with_target_graph_mut,
};
use super::super::super::{prune_scene_object_metadata, prune_scene_target_params, restore_scene_owner_graph};
use super::{
    FLUID_TYPE_ID, ROLE_SOURCE_TYPE_ID, collision_free_export_port, first_free_role_port,
    graph_level as level_ref,
    resolve_domain_ref, route_role_to_domain,
};

/// A discovered role source and every fluid destination reached by its authored
/// typed boundary route.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneFluidRoleAssignment {
    pub source: SceneNodeRef,
    pub source_doc_id: u32,
    pub name: String,
    pub domains: Vec<SceneNodeRef>,
}

#[derive(Debug, Clone)]
struct RoleRoute {
    source_group_id: u32,
    source_scope: Vec<u32>,
    role_id: u32,
    role_node_id: NodeId,
    export_ports: Vec<String>,
    edges: Vec<RouteEdge>,
    boundary_ports: Vec<BoundaryPort>,
    domains: Vec<SceneNodeRef>,
}

#[derive(Debug, Clone)]
struct RouteEdge {
    scope: Vec<u32>,
    wire: EffectGraphWire,
}

#[derive(Debug, Clone)]
struct BoundaryPort {
    scope: Vec<u32>,
    name: String,
}

/// Discover all direct FluidRole children of the selected root object group.
pub fn scene_fluid_role_assignments(
    def: &EffectGraphDef,
    group_doc_id: u32,
) -> Result<Vec<SceneFluidRoleAssignment>, String> {
    let group = def
        .nodes
        .iter()
        .find(|node| node.id == group_doc_id && node.type_id == GROUP_TYPE_ID)
        .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
    let body = group
        .group
        .as_deref()
        .ok_or_else(|| "Fluid role source group is malformed".to_string())?;
    let mut result = Vec::new();
    for role in body
        .nodes
        .iter()
        .filter(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
    {
        // Add Fluid owns an internal initial-fill source. Its local controls
        // remain on the fluid inspector; this list manages object assignments.
        let mut outgoing = body
            .wires
            .iter()
            .filter(|wire| wire.from_node == role.id)
            .peekable();
        if outgoing.peek().is_some()
            && outgoing.all(|wire| {
                body.nodes
                    .iter()
                    .any(|node| node.id == wire.to_node && node.type_id == FLUID_TYPE_ID)
            })
        {
            continue;
        }
        let source = SceneNodeRef {
            scope: vec![group.node_id.clone()],
            node: role.node_id.clone(),
        };
        let route = discover_role_route(def, &source)?;
        result.push(SceneFluidRoleAssignment {
            source,
            source_doc_id: role.id,
            name: role
                .handle
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Fluid Role".into()),
            domains: route.domains,
        });
    }
    Ok(result)
}

/// Remove one authored role and all of its typed destination routes.
#[derive(Debug)]
pub struct RemoveSceneFluidRoleCommand {
    target: GraphTarget,
    source: SceneNodeRef,
    catalog_default: EffectGraphDef,
    before: Option<EffectGraphDef>,
    after: Option<EffectGraphDef>,
    before_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    before_graph: Option<Option<EffectGraphDef>>,
    applied: bool,
    rejection: Option<String>,
}

impl RemoveSceneFluidRoleCommand {
    pub fn new(target: GraphTarget, source: SceneNodeRef, catalog_default: EffectGraphDef) -> Self {
        Self {
            target,
            source,
            catalog_default,
            before: None,
            after: None,
            before_instance: None,
            after_instance: None,
            before_graph: None,
            applied: false,
            rejection: None,
        }
    }
}

/// Replace every existing destination of one role with one selected domain.
#[derive(Debug)]
pub struct RetargetSceneFluidRoleCommand {
    target: GraphTarget,
    source: SceneNodeRef,
    domain: SceneNodeRef,
    catalog_default: EffectGraphDef,
    before: Option<EffectGraphDef>,
    after: Option<EffectGraphDef>,
    before_instance: Option<InstanceLayerSnapshot>,
    after_instance: Option<InstanceLayerSnapshot>,
    before_graph: Option<Option<EffectGraphDef>>,
    applied: bool,
    rejection: Option<String>,
}

impl RetargetSceneFluidRoleCommand {
    pub fn new(
        target: GraphTarget,
        source: SceneNodeRef,
        domain: SceneNodeRef,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            source,
            domain,
            catalog_default,
            before: None,
            after: None,
            before_instance: None,
            after_instance: None,
            before_graph: None,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for RemoveSceneFluidRoleCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        execute_lifecycle(
            project,
            &self.target,
            &self.catalog_default,
            &mut self.before,
            &mut self.after,
            &mut self.before_instance,
            &mut self.after_instance,
            &mut self.before_graph,
            &mut self.applied,
            &mut self.rejection,
            |candidate| remove_role(candidate, &self.source),
        );
    }

    fn undo(&mut self, project: &mut Project) {
        undo_lifecycle(
            project,
            &self.target,
            self.before_graph.clone(),
            self.before_instance.clone(),
            &mut self.applied,
        );
    }

    fn description(&self) -> &str {
        "Remove Fluid Role"
    }
    fn was_applied(&self) -> bool {
        self.applied
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

impl Command for RetargetSceneFluidRoleCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        let source = self.source.clone();
        let domain = self.domain.clone();
        execute_lifecycle(
            project,
            &self.target,
            &self.catalog_default,
            &mut self.before,
            &mut self.after,
            &mut self.before_instance,
            &mut self.after_instance,
            &mut self.before_graph,
            &mut self.applied,
            &mut self.rejection,
            |candidate| retarget_role(candidate, &source, &domain),
        );
    }

    fn undo(&mut self, project: &mut Project) {
        undo_lifecycle(
            project,
            &self.target,
            self.before_graph.clone(),
            self.before_instance.clone(),
            &mut self.applied,
        );
    }

    fn description(&self) -> &str {
        "Retarget Fluid Role"
    }
    fn was_applied(&self) -> bool {
        self.applied
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

fn execute_lifecycle(
    project: &mut Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    before: &mut Option<EffectGraphDef>,
    after: &mut Option<EffectGraphDef>,
    before_instance: &mut Option<InstanceLayerSnapshot>,
    after_instance: &mut Option<InstanceLayerSnapshot>,
    before_graph: &mut Option<Option<EffectGraphDef>>,
    applied: &mut bool,
    rejection: &mut Option<String>,
    mutate: impl FnOnce(&mut EffectGraphDef) -> Result<Vec<String>, String>,
) {
    *rejection = None;
    *applied = false;
    if let (Some(old), Some(new)) = (before.as_ref(), after.as_ref()) {
        if project.graph_for_target(target, Some(catalog_default)) != Some(old)
            || project.graph_target_owner(target).map(|owner| &owner.graph) != before_graph.as_ref()
        {
            *rejection =
                Some("Fluid role lifecycle redo rejected: graph changed since undo".into());
            return;
        }
        if with_target_graph_mut(project, target, catalog_default, true, |def| {
            *def = new.clone()
        })
        .is_none()
        {
            *rejection = Some("Fluid role lifecycle target is unavailable".into());
            return;
        }
        refresh_target_manifest(project, target);
        if let (Some(snapshot), Some(instance)) = (
            after_instance.clone(),
            project.graph_target_owner_mut(target),
        ) {
            snapshot.restore(instance);
        }
        *applied = true;
        return;
    }
    let Some(base) = project
        .graph_for_target(target, Some(catalog_default))
        .cloned()
    else {
        *rejection = Some("Fluid role lifecycle requires an existing graph".into());
        return;
    };
    let Some(previous_graph) = project
        .graph_target_owner(target)
        .map(|owner| owner.graph.clone())
    else {
        *rejection = Some("Fluid role lifecycle target is unavailable".into());
        return;
    };
    let Some(instance) = project.graph_target_owner_mut(target)
        .map(|instance| InstanceLayerSnapshot::capture(&*instance))
    else {
        *rejection = Some("Fluid role lifecycle target is unavailable".into());
        return;
    };
    let mut candidate = base.clone();
    let removed_params = match mutate(&mut candidate) {
        Ok(removed) => removed,
        Err(error) => {
            *rejection = Some(error);
            return;
        }
    };
    if with_target_graph_mut(project, target, catalog_default, true, |def| {
        *def = candidate.clone()
    })
    .is_none()
    {
        *rejection = Some("Fluid role lifecycle target is unavailable".into());
        return;
    }
    refresh_target_manifest(project, target);
    prune_scene_target_params(project, target, &removed_params);
    *before = Some(base);
    *after = Some(candidate);
    *before_graph = Some(previous_graph);
    *before_instance = Some(instance);
    *after_instance = project.graph_target_owner_mut(target)
        .map(|instance| InstanceLayerSnapshot::capture(&*instance));
    *applied = true;
}

fn undo_lifecycle(
    project: &mut Project,
    target: &GraphTarget,
    previous_graph: Option<Option<EffectGraphDef>>,
    instance: Option<InstanceLayerSnapshot>,
    applied: &mut bool,
) {
    if !*applied {
        return;
    }
    let Some(previous_graph) = previous_graph else {
        return;
    };
    restore_scene_owner_graph(project, target, previous_graph);
    refresh_target_manifest(project, target);
    if let (Some(snapshot), Some(instance)) = (instance, project.graph_target_owner_mut(target)) {
        snapshot.restore(instance);
    }
    *applied = false;
}

fn remove_role(def: &mut EffectGraphDef, source: &SceneNodeRef) -> Result<Vec<String>, String> {
    let route = discover_role_route(def, source)?;
    remove_route_edges(def, &route, true)?;
    Ok(prune_scene_object_metadata(
        def,
        std::slice::from_ref(&route.role_node_id),
    ))
}

fn retarget_role(
    def: &mut EffectGraphDef,
    source: &SceneNodeRef,
    domain: &SceneNodeRef,
) -> Result<Vec<String>, String> {
    let route = discover_role_route(def, source)?;
    remove_route_edges(def, &route, false)?;
    let (scope, fluid_id) = resolve_domain_ref(def, domain)?;
    if scope.first() == Some(&route.source_group_id) {
        return Err("A fluid role cannot target a fluid inside its own source group".into());
    }
    let source_port = route
        .export_ports
        .first()
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| create_export_port(def, route.source_group_id, route.role_id))?;
    collapse_export_ports(
        def,
        route.source_group_id,
        route.role_id,
        &source_port,
        &route.export_ports,
    )?;
    let mut next_id = super::super::max_node_id_over(&def.nodes)
        .checked_add(1)
        .ok_or_else(|| "Fluid role document id space is exhausted".to_string())?;
    let mut fresh_id = || {
        let id = next_id;
        next_id = next_id.checked_add(1)?;
        Some(id)
    };
    {
        let old_port = source_port;
        let export_port = collision_free_export_port(
            def,
            &scope,
            route.source_group_id,
            route.role_id,
            &old_port,
        )?;
        if export_port != old_port {
            rename_export_port(
                def,
                route.source_group_id,
                route.role_id,
                &old_port,
                &export_port,
            )?;
        }
        let target_port = first_free_role_port(def, &scope, fluid_id)?;
        route_role_to_domain(
            &mut def.nodes,
            &mut def.wires,
            &scope,
            route.source_group_id,
            export_port,
            target_port,
            fluid_id,
            &mut fresh_id,
        )?;
    }
    Ok(Vec::new())
}

fn create_export_port(
    def: &mut EffectGraphDef,
    source_group_id: u32,
    role_id: u32,
) -> Result<String, String> {
    let group = def
        .nodes
        .iter_mut()
        .find(|node| node.id == source_group_id && node.type_id == GROUP_TYPE_ID)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
    let outputs: Vec<_> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .collect();
    let [output] = outputs.as_slice() else {
        return Err(if outputs.is_empty() {
            "Fluid role source group has no output boundary".into()
        } else {
            "Fluid role source group requires one output boundary".into()
        });
    };
    let output_id = output.id;
    let mut suffix = 0u32;
    loop {
        let port = format!("fluid_role_source_{role_id}_retarget_{suffix}");
        if !group
            .interface
            .outputs
            .iter()
            .any(|output| output.name == port)
            && !group
                .wires
                .iter()
                .any(|wire| wire.to_node == output_id && wire.to_port == port)
        {
            group
                .interface
                .outputs
                .push(manifold_core::effect_graph_def::InterfacePortDef {
                    name: port.clone(),
                    port_type: "FluidRole".into(),
                });
            group
                .wires
                .push(scene_build_wire(role_id, "role", output_id, &port));
            return Ok(port);
        }
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| "Fluid role export port space is exhausted".to_string())?;
    }
}

fn collapse_export_ports(
    def: &mut EffectGraphDef,
    source_group_id: u32,
    role_id: u32,
    keep: &str,
    exports: &[String],
) -> Result<(), String> {
    let group = def
        .nodes
        .iter_mut()
        .find(|node| node.id == source_group_id && node.type_id == GROUP_TYPE_ID)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
    group
        .interface
        .outputs
        .retain(|port| !exports.iter().any(|name| name == &port.name) || port.name == keep);
    group.wires.retain(|wire| {
        !(wire.from_node == role_id
            && wire.from_port == "role"
            && exports.iter().any(|name| name == &wire.to_port)
            && wire.to_port != keep)
    });
    Ok(())
}

fn rename_export_port(
    def: &mut EffectGraphDef,
    source_group_id: u32,
    role_id: u32,
    old: &str,
    new: &str,
) -> Result<(), String> {
    let group = def
        .nodes
        .iter_mut()
        .find(|node| node.id == source_group_id && node.type_id == GROUP_TYPE_ID)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
    if group.interface.outputs.iter().any(|port| port.name == new) {
        return Err("Fluid role source export port already exists".into());
    }
    let output = group
        .interface
        .outputs
        .iter_mut()
        .find(|port| port.name == old && port.port_type == "FluidRole")
        .ok_or_else(|| "Fluid role source export port is unavailable".to_string())?;
    output.name = new.to_string();
    for wire in &mut group.wires {
        if wire.from_node == role_id && wire.from_port == "role" && wire.to_port == old {
            wire.to_port = new.to_string();
        }
    }
    Ok(())
}

fn discover_role_route(def: &EffectGraphDef, source: &SceneNodeRef) -> Result<RoleRoute, String> {
    if source.scope.len() != 1 {
        return Err("Fluid role source must be a direct child of one root object group".into());
    }
    let group = def
        .nodes
        .iter()
        .find(|node| node.node_id == source.scope[0] && node.type_id == GROUP_TYPE_ID)
        .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
    let body = group
        .group
        .as_deref()
        .ok_or_else(|| "Fluid role source group is malformed".to_string())?;
    let role = body
        .nodes
        .iter()
        .find(|node| node.node_id == source.node && node.type_id == ROLE_SOURCE_TYPE_ID)
        .ok_or_else(|| "Fluid role source is unavailable".to_string())?;
    let outputs: Vec<_> = body
        .nodes
        .iter()
        .filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .collect();
    let [output] = outputs.as_slice() else {
        return Err("Fluid role source group requires one output boundary".into());
    };
    let exports: Vec<_> = body
        .wires
        .iter()
        .filter(|wire| wire.from_node == role.id)
        .collect();
    if exports
        .iter()
        .any(|wire| wire.from_port != "role" || wire.to_node != output.id)
    {
        return Err("Fluid role source has malformed output routing".into());
    }
    let mut export_ports = Vec::new();
    for export in &exports {
        if body
            .interface
            .outputs
            .iter()
            .filter(|port| port.name == export.to_port)
            .count()
            != 1
            || !body
                .interface
                .outputs
                .iter()
                .any(|port| port.name == export.to_port && port.port_type == "FluidRole")
            || body
                .wires
                .iter()
                .filter(|wire| wire.to_node == output.id && wire.to_port == export.to_port)
                .count()
                != 1
            || export_ports.iter().any(|port| port == &export.to_port)
        {
            return Err("Fluid role source export has malformed typed output routing".into());
        }
        export_ports.push(export.to_port.clone());
    }
    let mut route = RoleRoute {
        source_group_id: group.id,
        source_scope: vec![group.id],
        role_id: role.id,
        role_node_id: role.node_id.clone(),
        export_ports: export_ports.clone(),
        edges: Vec::new(),
        boundary_ports: Vec::new(),
        domains: Vec::new(),
    };
    let mut visited = HashSet::new();
    for export_port in &export_ports {
        trace_connections(
            def.nodes.as_slice(),
            def.wires.as_slice(),
            &[],
            &[],
            group.id,
            export_port,
            &mut route,
            &mut visited,
        )?;
    }
    route.domains.sort_by(|left, right| {
        left.scope
            .iter()
            .map(NodeId::as_str)
            .cmp(right.scope.iter().map(NodeId::as_str))
            .then(left.node.as_str().cmp(right.node.as_str()))
    });
    route.domains.dedup();
    Ok(route)
}

fn trace_connections(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    runtime_scope: &[u32],
    stable_scope: &[NodeId],
    from_node: u32,
    from_port: &str,
    route: &mut RoleRoute,
    visited: &mut HashSet<(Vec<u32>, u32, String)>,
) -> Result<(), String> {
    let key = (runtime_scope.to_vec(), from_node, from_port.to_string());
    if !visited.insert(key) {
        return Err("Fluid role route contains a cycle".into());
    }
    let outgoing: Vec<_> = wires
        .iter()
        .filter(|wire| wire.from_node == from_node && wire.from_port == from_port)
        .cloned()
        .collect();
    for wire in outgoing {
        route.edges.push(RouteEdge {
            scope: runtime_scope.to_vec(),
            wire: wire.clone(),
        });
        let target = nodes
            .iter()
            .find(|node| node.id == wire.to_node)
            .ok_or_else(|| "Fluid role route targets a missing node".to_string())?;
        if target.type_id == FLUID_TYPE_ID {
            let Some(index) = wire
                .to_port
                .strip_prefix("role_")
                .and_then(|value| value.parse::<u32>().ok())
            else {
                return Err("Fluid role route targets an unsupported fluid port".into());
            };
            if index >= 64
                || wire.to_port != format!("role_{index}")
                || wires
                    .iter()
                    .filter(|candidate| {
                        candidate.to_node == target.id && candidate.to_port == wire.to_port
                    })
                    .count()
                    != 1
            {
                return Err("Fluid role route has multiple producers for a fluid role port".into());
            }
            route.domains.push(SceneNodeRef {
                scope: stable_scope.to_vec(),
                node: target.node_id.clone(),
            });
        } else if target.type_id == GROUP_TYPE_ID {
            if wires
                .iter()
                .filter(|candidate| {
                    candidate.to_node == target.id && candidate.to_port == wire.to_port
                })
                .count()
                != 1
            {
                return Err("Fluid role route has multiple producers for a group input".into());
            }
            let body = target
                .group
                .as_deref()
                .ok_or_else(|| "Fluid role route enters a malformed group".to_string())?;
            if body
                .interface
                .inputs
                .iter()
                .filter(|port| port.name == wire.to_port)
                .count()
                != 1
                || !body
                    .interface
                    .inputs
                    .iter()
                    .any(|port| port.name == wire.to_port && port.port_type == "FluidRole")
            {
                return Err("Fluid role route enters a malformed typed group input".into());
            }
            route.boundary_ports.push(BoundaryPort {
                scope: [runtime_scope, &[target.id]].concat(),
                name: wire.to_port.clone(),
            });
            let input_nodes: Vec<_> = body
                .nodes
                .iter()
                .filter(|node| node.type_id == GROUP_INPUT_TYPE_ID)
                .collect();
            if input_nodes.len() != 1 {
                return Err("Fluid role route requires one group input sentinel".into());
            }
            let next_scope = [runtime_scope, &[target.id]].concat();
            let next_stable = [stable_scope, std::slice::from_ref(&target.node_id)].concat();
            trace_connections(
                &body.nodes,
                &body.wires,
                &next_scope,
                &next_stable,
                input_nodes[0].id,
                &wire.to_port,
                route,
                visited,
            )?;
        } else {
            return Err("Fluid role route crosses an unsupported intermediary".into());
        }
    }
    visited.remove(&(runtime_scope.to_vec(), from_node, from_port.to_string()));
    Ok(())
}

fn remove_route_edges(
    def: &mut EffectGraphDef,
    route: &RoleRoute,
    remove_source: bool,
) -> Result<(), String> {
    for edge in &route.edges {
        let Some((_, wires)) = descend_level(&mut def.nodes, &mut def.wires, &edge.scope) else {
            return Err("Fluid role route scope is unavailable".into());
        };
        wires.retain(|wire| wire != &edge.wire);
    }
    for boundary in &route.boundary_ports {
        remove_boundary_input(def, boundary)?;
    }
    if remove_source {
        let Some((nodes, wires)) =
            descend_level(&mut def.nodes, &mut def.wires, &route.source_scope)
        else {
            return Err("Fluid role source scope is unavailable".into());
        };
        nodes.retain(|node| node.id != route.role_id);
        wires.retain(|wire| wire.from_node != route.role_id && wire.to_node != route.role_id);
        let group = def
            .nodes
            .iter_mut()
            .find(|node| node.id == route.source_group_id)
            .and_then(|node| node.group.as_deref_mut())
            .ok_or_else(|| "Fluid role source group is unavailable".to_string())?;
        group
            .interface
            .outputs
            .retain(|port| !route.export_ports.iter().any(|name| name == &port.name));
    }
    Ok(())
}

fn remove_boundary_input(def: &mut EffectGraphDef, boundary: &BoundaryPort) -> Result<(), String> {
    let Some(group_id) = boundary.scope.last().copied() else {
        return Err("Fluid role boundary scope is empty".into());
    };
    let parent_scope = &boundary.scope[..boundary.scope.len() - 1];
    let outer_live = level_ref(def, parent_scope).is_some_and(|(_, wires)| {
        wires
            .iter()
            .any(|wire| wire.to_node == group_id && wire.to_port == boundary.name)
    });
    let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &boundary.scope)
    else {
        return Err("Fluid role boundary scope is unavailable".into());
    };
    let input_ids: HashSet<u32> = nodes
        .iter()
        .filter(|node| node.type_id == GROUP_INPUT_TYPE_ID)
        .map(|node| node.id)
        .collect();
    let inner_live = wires
        .iter()
        .any(|wire| input_ids.contains(&wire.from_node) && wire.from_port == boundary.name);
    if !outer_live && !inner_live {
        let Some((parent_nodes, _)) = descend_level(&mut def.nodes, &mut def.wires, parent_scope)
        else {
            return Err("Fluid role boundary parent scope is unavailable".into());
        };
        let group = parent_nodes
            .iter_mut()
            .find(|node| node.id == group_id && node.type_id == GROUP_TYPE_ID)
            .ok_or_else(|| "Fluid role boundary group is unavailable".to_string())?;
        let body = group
            .group
            .as_deref_mut()
            .ok_or_else(|| "Fluid role boundary group is malformed".to_string())?;
        body.interface
            .inputs
            .retain(|port| port.name != boundary.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
