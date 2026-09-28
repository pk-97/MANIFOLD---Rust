use std::collections::BTreeMap;

use super::*;
use crate::commands::graph::resolve_target_instance;
use crate::command::Command;
use crate::commands::graph::test_support::{graph_of, project_with_graph};
use manifold_core::effect_graph_def::{
    BindingTarget, EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::{EffectId, GraphTarget, NodeId};

fn node(id: u32, node_id: &str, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.into(),
        handle: handle.map(str::to_owned),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.into(),
        to_node,
        to_port: to_port.into(),
    }
}

fn source_ref() -> SceneNodeRef {
    SceneNodeRef {
        scope: vec![NodeId::new("object_group")],
        node: NodeId::new("role"),
    }
}

fn nested_domain_ref() -> SceneNodeRef {
    SceneNodeRef {
        scope: vec![NodeId::new("domain_group")],
        node: NodeId::new("nested_fluid"),
    }
}

pub(super) fn role_graph(with_nested: bool, fanout: bool) -> EffectGraphDef {
    let mut render = node(0, "render", "node.render_scene", Some("Render"));
    render
        .params
        .insert("objects".into(), SerializedParamValue::Float { value: 1.0 });
    let role = node(50, "role", "node.fluid_role_source", Some("Inflow"));
    let object_group = GroupDef {
        interface: GroupInterface {
            inputs: vec![],
            outputs: vec![
                InterfacePortDef {
                    name: "object".into(),
                    port_type: "Object".into(),
                },
                InterfacePortDef {
                    name: "fluid_role_source_50".into(),
                    port_type: "FluidRole".into(),
                },
            ],
            params: vec![],
        },
        nodes: vec![
            role,
            node(51, "output", GROUP_OUTPUT_TYPE_ID, None),
            node(52, "object", "node.scene_object", Some("Object")),
            node(53, "transform", "node.transform_3d", Some("Transform")),
        ],
        wires: vec![
            wire(50, "role", 51, "fluid_role_source_50"),
            wire(52, "object", 51, "object"),
            wire(53, "transform", 50, "transform"),
        ],
        tint: None,
    };
    let mut object = node(10, "object_group", GROUP_TYPE_ID, Some("Object"));
    object.group = Some(Box::new(object_group));
    let mut nodes = vec![
        render,
        object,
        node(20, "fluid", "node.fluid_surface", Some("Fluid")),
    ];
    let mut wires = vec![
        wire(10, "object", 0, "object_0"),
        wire(10, "fluid_role_source_50", 20, "role_0"),
    ];
    if with_nested {
        let domain = GroupDef {
            interface: GroupInterface {
                inputs: vec![InterfacePortDef {
                    name: "fluid_role_source_50".into(),
                    port_type: "FluidRole".into(),
                }],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![
                node(60, "input", GROUP_INPUT_TYPE_ID, None),
                node(
                    31,
                    "nested_fluid",
                    "node.fluid_surface",
                    Some("Nested Fluid"),
                ),
                node(32, "output", GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![wire(60, "fluid_role_source_50", 31, "role_0")],
            tint: None,
        };
        let mut group = node(30, "domain_group", GROUP_TYPE_ID, Some("Domain"));
        group.group = Some(Box::new(domain));
        nodes.push(group);
        if fanout {
            wires.push(wire(10, "fluid_role_source_50", 30, "fluid_role_source_50"));
        }
    }
    let mut def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: vec![],
        nodes,
        wires,
    };
    if with_nested && fanout {
        def.preset_metadata = Some(PresetMetadata {
            id: manifold_core::PresetTypeId::from_string("roles".into()),
            display_name: "Roles".into(),
            category: "Geometry".into(),
            osc_prefix: "roles".into(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: vec![
                crate::commands::graph::test_support::slot("role_velocity", 1.0, true).spec,
            ],
            bindings: vec![manifold_core::effect_graph_def::BindingDef {
                id: "role_velocity".into(),
                label: "Velocity".into(),
                default_value: 1.0,
                target: BindingTarget::Node {
                    node_id: NodeId::new("role"),
                    param: "velocity_x".into(),
                },
                convert: Default::default(),
                user_added: true,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            }],
            param_aliases: vec![],
            value_aliases: vec![],
            string_params: vec![],
            string_bindings: vec![],
            scene_modifier: None,
            scene_bounds: None,
        });
    }
    def
}

fn execute_remove(
    graph: EffectGraphDef,
) -> (
    Project,
    EffectId,
    EffectGraphDef,
    EffectGraphDef,
    RemoveSceneFluidRoleCommand,
) {
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = RemoveSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        source_ref(),
        graph.clone(),
    );
    command.execute(&mut project);
    let after = graph_of(&project, &effect).clone();
    (project, effect, graph, after, command)
}

#[test]
fn scene_physics_role_lifecycle_discovery_returns_stable_destinations() {
    let graph = role_graph(true, true);
    let assignments = scene_fluid_role_assignments(&graph, 10).unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].source, source_ref());
    assert_eq!(assignments[0].source_doc_id, 50);
    assert_eq!(assignments[0].domains.len(), 2);
    assert!(assignments[0].domains.contains(&SceneNodeRef {
        scope: vec![],
        node: NodeId::new("fluid")
    }));
    assert!(assignments[0].domains.contains(&nested_domain_ref()));
}

#[test]
fn scene_physics_role_lifecycle_remove_preserves_other_graph_and_undoes_exactly() {
    let graph = role_graph(true, true);
    let (mut project, effect, before, after, mut command) = execute_remove(graph.clone());
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    assert!(
        after
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .all(|node| node.type_id != "node.fluid_role_source")
    );
    assert!(
        !after
            .wires
            .iter()
            .any(|wire| wire.from_port == "fluid_role_source_50")
    );
    assert!(
        after
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .interface
            .outputs
            .iter()
            .all(|port| port.name != "fluid_role_source_50")
    );
    assert!(manifold_core::flatten::flatten_groups(&after).is_ok());
    command.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &before);
    command.execute(&mut project);
    assert_eq!(graph_of(&project, &effect), &after);
}

#[test]
fn scene_physics_role_lifecycle_retarget_replaces_fanout_and_undoes() {
    let graph = role_graph(true, true);
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = RetargetSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        source_ref(),
        nested_domain_ref(),
        graph.clone(),
    );
    command.execute(&mut project);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect).clone();
    assert!(
        !after
            .wires
            .iter()
            .any(|wire| wire.to_node == 20 && wire.to_port == "role_0")
    );
    assert!(
        after
            .nodes
            .iter()
            .find(|node| node.id == 30)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .wires
            .iter()
            .any(|wire| wire.to_node == 31 && wire.to_port == "role_0")
    );
    command.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &graph);
}

#[test]
fn scene_physics_role_lifecycle_retarget_collapses_multiple_exports_to_one_route() {
    let mut graph = role_graph(true, true);
    let object = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap();
    object.interface.outputs.push(InterfacePortDef {
        name: "fluid_role_source_50_b".into(),
        port_type: "FluidRole".into(),
    });
    object
        .wires
        .push(wire(50, "role", 51, "fluid_role_source_50_b"));
    graph
        .wires
        .push(wire(10, "fluid_role_source_50_b", 20, "role_1"));
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = RetargetSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        source_ref(),
        nested_domain_ref(),
        graph,
    );
    command.execute(&mut project);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    let object = after
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    assert_eq!(
        object
            .interface
            .outputs
            .iter()
            .filter(|port| port.port_type == "FluidRole")
            .count(),
        1
    );
    assert_eq!(
        object
            .wires
            .iter()
            .filter(|wire| wire.from_node == 50 && wire.from_port == "role")
            .count(),
        1
    );
    let domain = after
        .nodes
        .iter()
        .find(|node| node.id == 30)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    assert_eq!(
        domain
            .wires
            .iter()
            .filter(|wire| wire.to_node == 31 && wire.to_port.starts_with("role_"))
            .count(),
        1
    );
}

#[test]
fn scene_physics_role_lifecycle_retarget_disconnected_role_creates_export() {
    let mut graph = role_graph(true, true);
    graph
        .wires
        .retain(|wire| wire.from_node != 10 || wire.from_port != "fluid_role_source_50");
    let body = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap();
    body.wires.retain(|wire| wire.from_node != 50);
    body.interface
        .outputs
        .retain(|port| port.port_type != "FluidRole");
    assert!(
        scene_fluid_role_assignments(&graph, 10).unwrap()[0]
            .domains
            .is_empty()
    );
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = RetargetSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        source_ref(),
        nested_domain_ref(),
        graph,
    );
    command.execute(&mut project);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    assert!(
        after
            .wires
            .iter()
            .any(|wire| wire.from_node == 10 && wire.to_node == 30)
    );
    assert!(
        after
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .interface
            .outputs
            .iter()
            .any(|port| port.port_type == "FluidRole")
    );
}

#[test]
fn scene_physics_role_lifecycle_rejects_malformed_route_atomically() {
    let mut graph = role_graph(false, false);
    graph.wires[1].to_port = "unsupported".into();
    let original = graph.clone();
    let (_project, _effect, _before, after, command) = execute_remove(graph);
    assert!(!command.was_applied());
    assert_eq!(after, original);
}

#[test]
fn scene_physics_role_lifecycle_rejects_duplicate_boundary_sentinels_atomically() {
    let mut source_duplicate = role_graph(false, false);
    source_duplicate
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap()
        .nodes
        .push(node(54, "second_output", GROUP_OUTPUT_TYPE_ID, None));
    let original = source_duplicate.clone();
    let (_project, _effect, _before, after, command) = execute_remove(source_duplicate);
    assert!(!command.was_applied());
    assert_eq!(after, original);

    let mut target_duplicate = role_graph(true, true);
    body_mut(&mut target_duplicate, 30)
        .nodes
        .push(node(61, "second_input", GROUP_INPUT_TYPE_ID, None));
    let original = target_duplicate.clone();
    let (_project, _effect, _before, after, command) = execute_remove(target_duplicate);
    assert!(!command.was_applied());
    assert_eq!(after, original);
}

pub(super) fn body_mut(graph: &mut EffectGraphDef, id: u32) -> &mut GroupDef {
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == id)
        .unwrap()
        .group
        .as_deref_mut()
        .unwrap()
}

fn add_second_source(graph: &mut EffectGraphDef) {
    let mut second = graph
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .clone();
    second.id = 110;
    second.node_id = NodeId::new("second_group");
    second.handle = Some("Second Object".into());
    let body = second.group.as_mut().unwrap();
    for node in &mut body.nodes {
        node.id += 100;
        node.node_id = NodeId::new(format!("second_{}", node.node_id));
    }
    for wire in &mut body.wires {
        wire.from_node += 100;
        wire.to_node += 100;
        if wire.to_port == "fluid_role_source_50" {
            wire.to_port = "second_role".into();
        }
    }
    body.interface
        .outputs
        .iter_mut()
        .find(|port| port.port_type == "FluidRole")
        .unwrap()
        .name = "second_role".into();
    graph.nodes.push(second);
    graph.wires.extend([
        wire(110, "object", 0, "object_1"),
        wire(110, "second_role", 30, "second_role"),
    ]);
    graph.nodes[0]
        .params
        .insert("objects".into(), SerializedParamValue::Float { value: 2.0 });
    let domain = body_mut(graph, 30);
    domain.interface.inputs.push(InterfacePortDef {
        name: "second_role".into(),
        port_type: "FluidRole".into(),
    });
    domain.wires.push(wire(60, "second_role", 31, "role_1"));
}

#[test]
fn scene_physics_role_lifecycle_remove_preserves_second_source_bindings_and_repeated_undo() {
    use manifold_core::effect_graph_def::StringBindingDef;
    use manifold_core::effects::ParameterDriver;
    let mut graph = role_graph(true, true);
    add_second_source(&mut graph);
    graph.preset_metadata.as_mut().unwrap().string_bindings = ["role", "second_role"]
        .into_iter()
        .map(|id| StringBindingDef {
            id: "model_file".into(),
            label: "Model File".into(),
            default_value: "assets/container.glb".into(),
            target: BindingTarget::Node {
                node_id: NodeId::new(id),
                param: "path".into(),
            },
        })
        .collect();
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let instance = resolve_target_instance(&target, &mut project).unwrap();
    instance.refresh_manifest_from_graph();
    instance.params.get_mut("role_velocity").unwrap().value = 0.75;
    instance.drivers = Some(vec![ParameterDriver::new(
        "role_velocity",
        Default::default(),
        Default::default(),
    )]);
    let before_params = instance.params.clone();
    let before_driver = serde_json::to_value(&instance.drivers).unwrap();
    let mut command = RemoveSceneFluidRoleCommand::new(target.clone(), source_ref(), graph.clone());
    let mut removed = None;
    for _ in 0..2 {
        command.execute(&mut project);
        assert!(command.was_applied(), "{:?}", command.rejection_reason());
        let after = graph_of(&project, &effect);
        assert!(manifold_core::flatten::flatten_groups(after).is_ok());
        assert_eq!(
            scene_fluid_role_assignments(after, 110).unwrap()[0].domains,
            vec![nested_domain_ref()]
        );
        let bindings = &after.preset_metadata.as_ref().unwrap().string_bindings;
        assert_eq!(bindings.len(), 1);
        assert!(
            matches!(&bindings[0].target, BindingTarget::Node { node_id, .. } if node_id == &NodeId::new("second_role"))
        );
        let domain = after
            .nodes
            .iter()
            .find(|node| node.id == 30)
            .unwrap()
            .group
            .as_ref()
            .unwrap();
        assert_eq!(domain.interface.inputs.len(), 1);
        assert_eq!(domain.interface.inputs[0].name, "second_role");
        let instance = project.graph_target_owner(&target).unwrap();
        assert!(instance.params.get("role_velocity").is_none());
        assert!(instance.drivers.is_none());
        if let Some(previous) = &removed {
            assert_eq!(after, previous);
        }
        removed = Some(after.clone());
        command.undo(&mut project);
        assert_eq!(graph_of(&project, &effect), &graph);
        let instance = project.graph_target_owner(&target).unwrap();
        assert_eq!(instance.params, before_params);
        assert_eq!(
            serde_json::to_value(&instance.drivers).unwrap(),
            before_driver
        );
    }
}

#[test]
fn scene_physics_role_lifecycle_retarget_avoids_existing_boundary_and_roundtrips() {
    let mut graph = role_graph(true, true);
    add_second_source(&mut graph);
    // The second source occupies a typed input with the first source's export name.
    let domain = body_mut(&mut graph, 30);
    domain
        .interface
        .inputs
        .iter_mut()
        .find(|p| p.name == "second_role")
        .unwrap()
        .name = "occupied".into();
    domain
        .wires
        .iter_mut()
        .find(|w| w.from_port == "second_role")
        .unwrap()
        .from_port = "occupied".into();
    graph
        .wires
        .iter_mut()
        .find(|w| w.from_node == 110 && w.to_node == 30)
        .unwrap()
        .to_port = "occupied".into();
    // Disconnect the first source from this boundary while keeping its root destination.
    graph
        .wires
        .retain(|w| !(w.from_node == 10 && w.to_node == 30));
    let domain = body_mut(&mut graph, 30);
    domain
        .wires
        .retain(|w| w.from_port != "fluid_role_source_50");
    domain
        .interface
        .inputs
        .retain(|p| p.name != "fluid_role_source_50");
    domain
        .interface
        .inputs
        .iter_mut()
        .find(|p| p.name == "occupied")
        .unwrap()
        .name = "fluid_role_source_50".into();
    domain
        .wires
        .iter_mut()
        .find(|w| w.from_port == "occupied")
        .unwrap()
        .from_port = "fluid_role_source_50".into();
    graph
        .wires
        .iter_mut()
        .find(|w| w.from_node == 110 && w.to_node == 30)
        .unwrap()
        .to_port = "fluid_role_source_50".into();
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = RetargetSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        source_ref(),
        nested_domain_ref(),
        graph.clone(),
    );
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    let after = graph_of(&project, &effect).clone();
    assert!(manifold_core::flatten::flatten_groups(&after).is_ok());
    assert_eq!(
        scene_fluid_role_assignments(&after, 10).unwrap()[0].domains,
        vec![nested_domain_ref()]
    );
    assert_eq!(
        scene_fluid_role_assignments(&after, 110).unwrap()[0].domains,
        vec![nested_domain_ref()]
    );
    assert_eq!(after.wires.iter().filter(|w| w.to_node == 30).count(), 2);
    let reloaded: Project =
        serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(graph_of(&reloaded, &effect), &after);
    command.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &graph);
    command.execute(&mut project);
    assert_eq!(graph_of(&project, &effect), &after);
}

#[test]
fn scene_physics_role_lifecycle_full_domain_and_tracking_are_atomic() {
    let mut graph = role_graph(true, false);
    let domain = body_mut(&mut graph, 30);
    domain.interface.inputs.clear();
    domain.wires.clear();
    for slot in 0..64 {
        domain.nodes.push(node(
            200 + slot,
            &format!("filler_{slot}"),
            ROLE_SOURCE_TYPE_ID,
            None,
        ));
        domain
            .wires
            .push(wire(200 + slot, "role", 31, &format!("role_{slot}")));
    }
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    resolve_target_instance(&target, &mut project)
        .unwrap()
        .graph = None;
    let before = serde_json::to_value(&project).unwrap();
    let mut command = RetargetSceneFluidRoleCommand::new(
        target.clone(),
        source_ref(),
        nested_domain_ref(),
        graph,
    );
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert!(
        command
            .rejection_reason()
            .unwrap()
            .contains("no free role ports")
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    let instance = project.graph_target_owner(&target).unwrap();
    assert_eq!(instance.graph_version, 0);
    assert_eq!(instance.graph_structure_version, 0);

    let mut graph = role_graph(false, false);
    for slot in 1..64 {
        graph.nodes.push(node(
            200 + slot,
            &format!("filler_{slot}"),
            ROLE_SOURCE_TYPE_ID,
            None,
        ));
        graph
            .wires
            .push(wire(200 + slot, "role", 20, &format!("role_{slot}")));
    }
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    resolve_target_instance(&target, &mut project)
        .unwrap()
        .graph = None;
    let mut same = RetargetSceneFluidRoleCommand::new(
        target.clone(),
        source_ref(),
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid"),
        },
        graph,
    );
    same.execute(&mut project);
    assert!(same.was_applied(), "{:?}", same.rejection_reason());
    assert_eq!(
        graph_of(&project, &effect)
            .wires
            .iter()
            .filter(|w| w.to_node == 20)
            .count(),
        64
    );
    same.undo(&mut project);
    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
}

#[test]
fn scene_physics_role_lifecycle_malformed_producers_and_stale_refs_preserve_state() {
    for case in 0..7 {
        let mut graph = role_graph(true, true);
        match case {
            0 => body_mut(&mut graph, 10)
                .wires
                .push(wire(52, "role", 51, "fluid_role_source_50")),
            1 => graph
                .wires
                .push(wire(20, "role", 30, "fluid_role_source_50")),
            2 => body_mut(&mut graph, 30)
                .wires
                .push(wire(32, "role", 31, "role_0")),
            3 => graph.wires[1].to_port = "role_64".into(),
            4 => graph.wires[1].to_port = "role_01".into(),
            5 => body_mut(&mut graph, 10)
                .wires
                .push(wire(50, "role", 52, "unsupported")),
            _ => {}
        }
        let (mut project, effect) = project_with_graph(graph.clone());
        let target = GraphTarget::Effect(effect);
        resolve_target_instance(&target, &mut project)
            .unwrap()
            .graph = None;
        let before = serde_json::to_value(&project).unwrap();
        let source = if case == 6 {
            SceneNodeRef {
                scope: vec![NodeId::new("gone")],
                node: NodeId::new("role"),
            }
        } else {
            source_ref()
        };
        let mut command = RemoveSceneFluidRoleCommand::new(target.clone(), source, graph);
        command.execute(&mut project);
        assert!(!command.was_applied(), "case {case}");
        assert!(command.rejection_reason().is_some());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        let instance = project.graph_target_owner(&target).unwrap();
        assert_eq!(
            (instance.graph_version, instance.graph_structure_version),
            (0, 0)
        );
    }
}

#[test]
fn scene_physics_role_lifecycle_rejects_self_target_and_stale_redo() {
    let mut graph = role_graph(false, false);
    body_mut(&mut graph, 10)
        .nodes
        .push(node(55, "local_fluid", FLUID_TYPE_ID, None));
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let mut retarget = RetargetSceneFluidRoleCommand::new(
        target.clone(),
        source_ref(),
        SceneNodeRef {
            scope: vec![NodeId::new("object_group")],
            node: NodeId::new("local_fluid"),
        },
        graph.clone(),
    );
    retarget.execute(&mut project);
    assert!(!retarget.was_applied());
    assert_eq!(graph_of(&project, &effect), &graph);
    assert_eq!(
        project.graph_target_owner(&target).unwrap().graph_version,
        0
    );
    let mut remove = RemoveSceneFluidRoleCommand::new(target.clone(), source_ref(), graph);
    remove.execute(&mut project);
    assert!(remove.was_applied());
    remove.undo(&mut project);
    let instance = resolve_target_instance(&target, &mut project).unwrap();
    instance.graph.as_mut().unwrap().description = Some("Changed after undo".into());
    let before = serde_json::to_value(&project).unwrap();
    let version = project.graph_target_owner(&target).unwrap().graph_version;
    remove.execute(&mut project);
    assert!(!remove.was_applied());
    assert!(
        remove
            .rejection_reason()
            .unwrap()
            .contains("changed since undo")
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert_eq!(
        project.graph_target_owner(&target).unwrap().graph_version,
        version
    );
}
