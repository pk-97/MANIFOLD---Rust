use std::collections::BTreeMap;

use super::*;
use crate::commands::graph::test_support::{graph_of, project_with_graph};
use manifold_core::effect_graph_def::{
    BindingTarget, EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef,
    PresetMetadata, SerializedParamValue, StringBindingDef,
};
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
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

fn cube_graph(nested: bool) -> EffectGraphDef {
    let mut render = node(0, "render", RENDER_SCENE_TYPE_ID, Some("Render"));
    render
        .params
        .insert("objects".into(), SerializedParamValue::Float { value: 1.0 });
    let mut cube = node(12, "cube", CUBE_MESH_TYPE_ID, Some("Cube Mesh"));
    cube.params
        .insert("size".into(), SerializedParamValue::Float { value: 2.0 });
    let group = GroupDef {
        interface: GroupInterface {
            inputs: vec![],
            outputs: vec![InterfacePortDef {
                name: "object".into(),
                port_type: "Object".into(),
            }],
            params: vec![],
        },
        nodes: vec![
            node(11, "transform", "node.transform_3d", Some("Transform")),
            cube,
            node(13, "material", "node.pbr_material", Some("Material")),
            node(14, "object", "node.scene_object", Some("Object")),
            node(15, "output", GROUP_OUTPUT_TYPE_ID, None),
        ],
        wires: vec![
            wire(11, "transform", 14, "transform"),
            wire(12, "vertices", 14, "vertices"),
            wire(13, "out", 14, "material"),
            wire(14, "object", 15, "object"),
        ],
        tint: None,
    };
    let mut object_group = node(10, "object_group", GROUP_TYPE_ID, Some("Object"));
    object_group.group = Some(Box::new(group));
    let mut nodes = vec![
        render,
        object_group,
        node(20, "fluid", FLIP_DOMAIN_TYPE_ID, Some("Fluid")),
    ];
    let wires = vec![wire(10, "object", 0, "object_0")];
    if nested {
        let domain = GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![
                node(31, "nested_fluid", FLIP_DOMAIN_TYPE_ID, Some("Nested Fluid")),
                node(32, "domain_output", GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![],
            tint: None,
        };
        let mut domain_group = node(30, "domain_group", GROUP_TYPE_ID, Some("Domain"));
        domain_group.group = Some(Box::new(domain));
        nodes.push(domain_group);
    }
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: vec![],
        nodes,
        wires,
    }
}

fn imported_compound_graph(two_fluids_in_boundary: bool) -> EffectGraphDef {
    let mut render = node(0, "render", RENDER_SCENE_TYPE_ID, Some("Render"));
    render
        .params
        .insert("objects".into(), SerializedParamValue::Float { value: 2.0 });
    let mut source_a = node(12, "source_a", GLTF_MESH_TYPE_ID, Some("Source A"));
    source_a.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: "asset.glb".into(),
        },
    );
    source_a.params.insert(
        "material_index".into(),
        SerializedParamValue::Int { value: 2 },
    );
    let mut source_b = node(19, "source_b", GLTF_MESH_TYPE_ID, Some("Source B"));
    source_b.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: "asset.glb".into(),
        },
    );
    source_b.params.insert(
        "material_index".into(),
        SerializedParamValue::Int { value: 5 },
    );
    let output_id = 15;
    let group = GroupDef {
        interface: GroupInterface {
            inputs: vec![],
            outputs: vec![
                InterfacePortDef {
                    name: "object".into(),
                    port_type: "Object".into(),
                },
                InterfacePortDef {
                    name: "object_1".into(),
                    port_type: "Object".into(),
                },
            ],
            params: vec![],
        },
        nodes: vec![
            node(11, "parent", "node.transform_3d", Some("Parent")),
            node(17, "part_transform_a", "node.transform_3d", Some("Part A")),
            node(18, "part_transform_b", "node.transform_3d", Some("Part B")),
            source_a,
            source_b,
            node(13, "material_a", "node.pbr_material", Some("Material A")),
            node(20, "material_b", "node.pbr_material", Some("Material B")),
            node(14, "object_a", "node.scene_object", Some("Part A")),
            node(16, "object_b", "node.scene_object", Some("Part B")),
            node(output_id, "output", GROUP_OUTPUT_TYPE_ID, None),
        ],
        wires: vec![
            wire(11, "transform", 14, "parent_transform"),
            wire(11, "transform", 16, "parent_transform"),
            wire(17, "transform", 14, "transform"),
            wire(18, "transform", 16, "transform"),
            wire(12, "vertices", 14, "vertices"),
            wire(19, "vertices", 16, "vertices"),
            wire(13, "out", 14, "material"),
            wire(20, "out", 16, "material"),
            wire(14, "object", output_id, "object"),
            wire(16, "object", output_id, "object_1"),
        ],
        tint: None,
    };
    let mut object_group = node(10, "object_group", GROUP_TYPE_ID, Some("Imported"));
    object_group.group = Some(Box::new(group));
    let mut nodes = vec![
        render,
        object_group,
        node(30, "fluid_a", FLIP_DOMAIN_TYPE_ID, Some("Fluid A")),
    ];
    let wires = vec![
        wire(10, "object", 0, "object_0"),
        wire(10, "object_1", 0, "object_1"),
    ];
    if two_fluids_in_boundary {
        let domain = GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![
                node(41, "fluid_b", FLIP_DOMAIN_TYPE_ID, Some("Fluid B")),
                node(42, "fluid_c", FLIP_DOMAIN_TYPE_ID, Some("Fluid C")),
                node(43, "output", GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![],
            tint: None,
        };
        let mut domain_group = node(40, "domain_group", GROUP_TYPE_ID, Some("Domain"));
        domain_group.group = Some(Box::new(domain));
        nodes.push(domain_group);
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
    def.preset_metadata = Some(PresetMetadata {
        id: manifold_core::PresetTypeId::from_string("Imported".into()),
        display_name: "Imported".into(),
        category: "Geometry".into(),
        osc_prefix: "imported".into(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: vec![],
        bindings: vec![],
        param_aliases: vec![],
        value_aliases: vec![],
        string_params: vec![],
        string_bindings: vec![StringBindingDef {
            id: "asset".into(),
            label: "Asset".into(),
            default_value: "asset.glb".into(),
            target: BindingTarget::Node {
                node_id: NodeId::new("source_a"),
                param: "path".into(),
            },
        }],
        scene_modifier: None,
        scene_bounds: None,
    });
    def
}

fn run_command(
    graph: EffectGraphDef,
    nested: bool,
) -> (Project, EffectId, AssignSceneFluidRoleCommand) {
    let (mut project, effect) = project_with_graph(graph.clone());
    let domain = SceneNodeRef {
        scope: if nested {
            vec![NodeId::new("domain_group")]
        } else {
            vec![]
        },
        node: NodeId::new(if nested { "nested_fluid" } else { "fluid" }),
    };
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        domain,
        1,
        vec![],
        graph,
    );
    command.execute(&mut project);
    (project, effect, command)
}

#[test]
fn scene_physics_assign_role_cube_preserves_size_and_undo_redo() {
    let graph = cube_graph(false);
    let (mut project, effect, mut command) = run_command(graph.clone(), false);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect).clone();
    let body = after
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    let role = body
        .nodes
        .iter()
        .find(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
        .unwrap();
    assert_eq!(role.params.get("radius"), Some(&float(1.0)));
    assert!(body.wires.iter().any(|wire| wire.from_node == 12
        && wire.from_port == "source"
        && wire.to_node == role.id
        && wire.to_port == "mesh_0"));
    assert!(
        after
            .wires
            .iter()
            .any(|wire| wire.to_node == 20 && wire.to_port == "role_0")
    );
    command.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &graph);
    command.execute(&mut project);
    assert_eq!(graph_of(&project, &effect), &after);
}

#[test]
fn scene_physics_assign_role_platonic_wires_visible_mesh_source() {
    let mut graph = cube_graph(false);
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| node.id == 12)
        .unwrap()
        .type_id = "node.platonic_solid_mesh".into();
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid"),
        },
        1,
        vec![],
        graph,
    );
    command.execute(&mut project);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    let body = after
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    let role = body
        .nodes
        .iter()
        .find(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
        .unwrap();
    assert!(body.wires.iter().any(|wire| wire.from_node == 12
        && wire.from_port == "source"
        && wire.to_node == role.id
        && wire.to_port == "mesh_0"));
}

#[test]
fn scene_physics_assign_role_nested_domain_creates_typed_boundary() {
    let (project, effect, command) = run_command(cube_graph(true), true);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let graph = graph_of(&project, &effect);
    let domain = graph
        .nodes
        .iter()
        .find(|node| node.id == 30)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    assert!(
        domain
            .interface
            .inputs
            .iter()
            .any(|port| port.port_type == "FluidRole")
    );
    assert!(
        domain
            .wires
            .iter()
            .any(|wire| wire.to_node == 31 && wire.to_port == "role_0")
    );
}

#[test]
fn scene_physics_assign_role_rejects_invalid_role_atomically() {
    let graph = cube_graph(false);
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid"),
        },
        4,
        vec![],
        graph.clone(),
    );
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert_eq!(graph_of(&project, &effect), &graph);
}

#[test]
fn scene_physics_assign_role_imported_preserves_binding_parent_pose_and_parts() {
    let graph = imported_compound_graph(false);
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid_a"),
        },
        2,
        vec![],
        graph,
    );
    command.execute(&mut project);
    assert!(
        command.was_applied(),
        "rejected: {:?}",
        command.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    let body = after
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    let role = body
        .nodes
        .iter()
        .find(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
        .unwrap();
    assert!(role.node_id.as_str().starts_with("fluid_role_source_"));
    assert!(
        !body
            .wires
            .iter()
            .any(|wire| wire.to_node == role.id && wire.to_port == "source_transform")
    );
    assert_eq!(
        body.wires
            .iter()
            .filter(|wire| wire.to_node == role.id && wire.to_port.starts_with("part_"))
            .count(),
        2
    );
    assert_eq!(
        body.wires
            .iter()
            .filter(|wire| wire.to_node == role.id && wire.to_port.starts_with("mesh_"))
            .map(|wire| (wire.from_node, wire.to_port.clone()))
            .collect::<Vec<_>>(),
        vec![(12, "mesh_0".into()), (19, "mesh_1".into())]
    );
    assert!(!role.params.contains_key("path"));
    assert!(!role.params.contains_key("compound_materials"));
    assert!(after.preset_metadata.as_ref().unwrap().string_bindings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &NodeId::new("source_a") && param == "path")));
    assert!(!after.preset_metadata.as_ref().unwrap().string_bindings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &role.node_id && param == "path")));
}

#[test]
fn scene_physics_assign_role_repeated_assignments_use_distinct_source_ports() {
    let graph = imported_compound_graph(false);
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let mut first = AssignSceneFluidRoleCommand::new(
        target.clone(),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid_a"),
        },
        1,
        vec![],
        graph.clone(),
    );
    first.execute(&mut project);
    assert!(
        first.was_applied(),
        "rejected: {:?}",
        first.rejection_reason()
    );
    let mut second = AssignSceneFluidRoleCommand::new(
        target,
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid_a"),
        },
        3,
        vec![],
        graph,
    );
    second.execute(&mut project);
    assert!(
        second.was_applied(),
        "rejected: {:?}",
        second.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    let body = after
        .nodes
        .iter()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    let ports: Vec<_> = body
        .interface
        .outputs
        .iter()
        .filter(|port| port.port_type == "FluidRole")
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(ports.len(), 2);
    assert_ne!(ports[0], ports[1]);
    let fluid_wires: Vec<_> = after
        .wires
        .iter()
        .filter(|wire| wire.to_node == 30 && wire.to_port.starts_with("role_"))
        .collect();
    assert_eq!(fluid_wires.len(), 2);
    assert_ne!(fluid_wires[0].from_port, fluid_wires[1].from_port);
}

#[test]
fn scene_physics_assign_role_two_fluids_share_boundary_without_collisions() {
    let graph = imported_compound_graph(true);
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let scope = vec![NodeId::new("domain_group")];
    let mut first = AssignSceneFluidRoleCommand::new(
        target.clone(),
        0,
        0,
        SceneNodeRef {
            scope: scope.clone(),
            node: NodeId::new("fluid_b"),
        },
        1,
        vec![],
        graph.clone(),
    );
    first.execute(&mut project);
    assert!(
        first.was_applied(),
        "rejected: {:?}",
        first.rejection_reason()
    );
    let mut second = AssignSceneFluidRoleCommand::new(
        target,
        0,
        0,
        SceneNodeRef {
            scope,
            node: NodeId::new("fluid_c"),
        },
        1,
        vec![],
        graph,
    );
    second.execute(&mut project);
    assert!(
        second.was_applied(),
        "rejected: {:?}",
        second.rejection_reason()
    );
    let after = graph_of(&project, &effect);
    let domain = after
        .nodes
        .iter()
        .find(|node| node.id == 40)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    assert_eq!(
        domain
            .interface
            .inputs
            .iter()
            .filter(|port| port.port_type == "FluidRole")
            .count(),
        2
    );
    assert!(
        domain
            .wires
            .iter()
            .any(|wire| wire.to_node == 41 && wire.to_port == "role_0")
    );
    assert!(
        domain
            .wires
            .iter()
            .any(|wire| wire.to_node == 42 && wire.to_port == "role_0")
    );
}

#[test]
fn scene_physics_assign_role_allocates_around_each_boundary_collision() {
    for case in [
        "free",
        "source_reserved",
        "target_reserved",
        "existing_producer",
    ] {
        let mut graph = imported_compound_graph(true);
        if case == "existing_producer" {
            let body = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == 10)
                .unwrap()
                .group
                .as_mut()
                .unwrap();
            body.nodes.push(node(
                50,
                "existing_role",
                ROLE_SOURCE_TYPE_ID,
                Some("Existing"),
            ));
        }
        let reserved = format!("fluid_role_source_{}", max_node_id_over(&graph.nodes) + 1);
        {
            let source_body = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == 10)
                .unwrap()
                .group
                .as_mut()
                .unwrap();
            if case == "source_reserved" || case == "existing_producer" {
                source_body.interface.outputs.push(InterfacePortDef {
                    name: reserved.clone(),
                    port_type: "FluidRole".into(),
                });
                if case == "existing_producer" {
                    source_body.wires.push(wire(50, "role", 15, &reserved));
                }
            }
        }
        if case == "target_reserved" {
            graph
                .nodes
                .iter_mut()
                .find(|node| node.id == 40)
                .unwrap()
                .group
                .as_mut()
                .unwrap()
                .interface
                .inputs
                .push(InterfacePortDef {
                    name: reserved.clone(),
                    port_type: "FluidRole".into(),
                });
        }
        let original_root_wires = graph.wires.clone();
        let original_source_wires = graph
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .wires
            .clone();
        let original_target_wires = graph
            .nodes
            .iter()
            .find(|node| node.id == 40)
            .unwrap()
            .group
            .as_ref()
            .unwrap()
            .wires
            .clone();
        let (mut project, effect) = project_with_graph(graph.clone());
        let mut command = AssignSceneFluidRoleCommand::new(
            GraphTarget::Effect(effect.clone()),
            0,
            0,
            SceneNodeRef {
                scope: vec![NodeId::new("domain_group")],
                node: NodeId::new("fluid_b"),
            },
            1,
            vec![],
            graph,
        );
        command.execute(&mut project);
        assert!(
            command.was_applied(),
            "{case}: {:?}",
            command.rejection_reason()
        );
        let after = graph_of(&project, &effect);
        assert!(
            original_root_wires
                .iter()
                .all(|wire| after.wires.contains(wire))
        );
        let source_body = after
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .group
            .as_ref()
            .unwrap();
        assert!(
            original_source_wires
                .iter()
                .all(|wire| source_body.wires.contains(wire))
        );
        let target_body = after
            .nodes
            .iter()
            .find(|node| node.id == 40)
            .unwrap()
            .group
            .as_ref()
            .unwrap();
        assert!(
            original_target_wires
                .iter()
                .all(|wire| target_body.wires.contains(wire))
        );
        let allocated = after
            .wires
            .iter()
            .find(|wire| wire.from_node == 10 && wire.to_node == 40)
            .unwrap()
            .from_port
            .clone();
        assert_eq!(allocated == reserved, case == "free");
        assert!(
            source_body
                .interface
                .outputs
                .iter()
                .any(|port| port.name == allocated && port.port_type == "FluidRole")
        );
        assert!(
            manifold_core::flatten::flatten_groups(after).is_ok(),
            "{case}"
        );
    }
}

#[test]
fn scene_physics_assign_role_rejects_duplicate_target_input_sentinels_atomically() {
    let mut graph = imported_compound_graph(true);
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 40)
        .unwrap()
        .group
        .as_mut()
        .unwrap()
        .nodes
        .extend([
            node(44, "first_input", GROUP_INPUT_TYPE_ID, None),
            node(45, "second_input", GROUP_INPUT_TYPE_ID, None),
        ]);
    let (mut project, effect) = project_with_graph(graph.clone());
    let before = serde_json::to_value(&project).unwrap();
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![NodeId::new("domain_group")],
            node: NodeId::new("fluid_b"),
        },
        1,
        vec![],
        graph,
    );
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert!(command.rejection_reason().is_some());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
}

#[test]
fn scene_physics_assign_role_rejects_duplicate_source_output_sentinels_atomically() {
    let mut graph = imported_compound_graph(false);
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap()
        .nodes
        .extend([
            node(44, "first_output", GROUP_OUTPUT_TYPE_ID, None),
            node(45, "second_output", GROUP_OUTPUT_TYPE_ID, None),
        ]);
    let (mut project, effect) = project_with_graph(graph.clone());
    let before = serde_json::to_value(&project).unwrap();
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid_a"),
        },
        1,
        vec![],
        graph,
    );
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert!(command.rejection_reason().is_some());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
}

#[test]
fn scene_physics_assign_role_rejects_nonshared_compound_pose_atomically() {
    let mut graph = imported_compound_graph(false);
    let body = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_mut()
        .unwrap();
    body.wires
        .retain(|wire| !(wire.to_node == 16 && wire.to_port == "parent_transform"));
    let original = graph.clone();
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut command = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![],
            node: NodeId::new("fluid_a"),
        },
        1,
        vec![],
        graph,
    );
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert_eq!(graph_of(&project, &effect), &original);
}

/// Role assignment recognizes a domain by `is_liquid_domain`, so a GPU
/// template's simulation node takes a role with no per-solver code.
#[test]
fn scene_physics_assign_role_accepts_gpu_domain() {
    let mut graph = cube_graph(false);
    graph.nodes.iter_mut().find(|node| node.id == 20).unwrap().type_id =
        manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID.into();
    let (_project, _effect, command) = run_command(graph, false);
    assert!(command.was_applied(), "rejected: {:?}", command.rejection_reason());
}
