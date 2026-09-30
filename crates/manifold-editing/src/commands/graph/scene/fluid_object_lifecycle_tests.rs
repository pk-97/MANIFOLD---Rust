use std::collections::BTreeMap;

use super::*;
use crate::command::Command;
use crate::commands::graph::test_support::{graph_of, mirror_catalog_default, project_with_graph};
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID,
    GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef,
    SerializedParamValue,
};
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::{GraphTarget, NodeId};

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

fn assigned_object_graph() -> EffectGraphDef {
    let mut render = node(0, "render", "node.render_scene", Some("Render"));
    render
        .params
        .insert("objects".into(), SerializedParamValue::Float { value: 1.0 });

    let object_group = GroupDef {
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
            node(14, "cube", "node.cube_mesh", Some("Cube Mesh")),
            node(12, "object", "node.scene_object", Some("Object")),
            node(13, "output", GROUP_OUTPUT_TYPE_ID, None),
        ],
        wires: vec![
            wire(11, "transform", 12, "transform"),
            wire(14, "vertices", 12, "vertices"),
            wire(12, "object", 13, "object"),
        ],
        tint: None,
    };
    let mut object = node(10, "object_group", GROUP_TYPE_ID, Some("Object"));
    object.group = Some(Box::new(object_group));

    let domain_group = GroupDef {
        interface: GroupInterface {
            inputs: vec![],
            outputs: vec![],
            params: vec![],
        },
        nodes: vec![
            node(60, "input", GROUP_INPUT_TYPE_ID, None),
            node(
                61,
                "nested_fluid",
                FLIP_DOMAIN_TYPE_ID,
                Some("Nested Fluid"),
            ),
            node(62, "output", GROUP_OUTPUT_TYPE_ID, None),
        ],
        wires: vec![],
        tint: None,
    };
    let mut domain = node(30, "domain_group", GROUP_TYPE_ID, Some("Domain"));
    domain.group = Some(Box::new(domain_group));

    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: vec![],
        nodes: vec![render, object, domain],
        wires: vec![wire(10, "object", 0, "object_0")],
    }
}

#[test]
fn scene_physics_object_fluid_duplicate_remove_preserves_independent_routes() {
    let graph = assigned_object_graph();
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let mut assign = AssignSceneFluidRoleCommand::new(
        target.clone(),
        0,
        0,
        SceneNodeRef {
            scope: vec![NodeId::new("domain_group")],
            node: NodeId::new("nested_fluid"),
        },
        1,
        vec![],
        graph.clone(),
    );
    assign.execute(&mut project);
    assert!(
        assign.was_applied(),
        "assignment rejected: {:?}",
        assign.rejection_reason()
    );
    let assigned = graph_of(&project, &effect).clone();

    let mut duplicate =
        DuplicateSceneObjectCommand::new(target.clone(), vec![], 0, 0, mirror_catalog_default());
    duplicate.execute(&mut project);
    assert!(
        duplicate.was_applied(),
        "duplicate rejected: {:?}",
        duplicate.rejection_reason()
    );
    let duplicated = graph_of(&project, &effect).clone();
    let clone_id = duplicated
        .nodes
        .iter()
        .find(|node| node.handle.as_deref() == Some("Object 2"))
        .expect("duplicated object group")
        .id;
    assert_eq!(
        scene_fluid_role_assignments(&duplicated, clone_id)
            .unwrap()
            .len(),
        1
    );
    assert!(manifold_core::flatten::flatten_groups(&duplicated).is_ok());

    duplicate.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &assigned);
    duplicate.execute(&mut project);
    assert_eq!(graph_of(&project, &effect), &duplicated);

    let mut remove = RemoveSceneObjectCommand::new(target, vec![], 0, 0, mirror_catalog_default());
    remove.execute(&mut project);
    assert!(
        remove.was_applied(),
        "remove rejected: {:?}",
        remove.rejection_reason()
    );
    let after_remove = graph_of(&project, &effect).clone();
    let remaining_id = after_remove
        .nodes
        .iter()
        .find(|node| node.handle.as_deref() == Some("Object 2"))
        .map(|node| node.id)
        .expect("duplicate survives removal");
    assert_eq!(
        scene_fluid_role_assignments(&after_remove, remaining_id)
            .unwrap()
            .len(),
        1
    );
    assert!(manifold_core::flatten::flatten_groups(&after_remove).is_ok());
    remove.undo(&mut project);
    assert_eq!(graph_of(&project, &effect), &duplicated);
}

#[test]
fn scene_physics_object_fluid_duplicate_rejects_missing_source_atomically() {
    let graph = assigned_object_graph();
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect.clone());
    let version = project
        .graph_target_owner(&target)
        .unwrap()
        .graph_structure_version;
    let mut duplicate =
        DuplicateSceneObjectCommand::new(target, vec![], 0, 7, mirror_catalog_default());
    duplicate.execute(&mut project);
    assert!(!duplicate.was_applied());
    assert_eq!(graph_of(&project, &effect), &graph);
    assert_eq!(
        project
            .graph_target_owner(&GraphTarget::Effect(effect))
            .unwrap()
            .graph_structure_version,
        version
    );
}

fn with_assigned_role() -> EffectGraphDef {
    let graph = assigned_object_graph();
    let (mut project, effect) = project_with_graph(graph.clone());
    let mut assign = AssignSceneFluidRoleCommand::new(
        GraphTarget::Effect(effect.clone()),
        0,
        0,
        SceneNodeRef {
            scope: vec![NodeId::new("domain_group")],
            node: NodeId::new("nested_fluid"),
        },
        1,
        vec![],
        graph,
    );
    assign.execute(&mut project);
    assert!(assign.was_applied(), "{:?}", assign.rejection_reason());
    graph_of(&project, &effect).clone()
}

#[test]
fn scene_physics_object_fluid_full_domain_rejects_without_materializing() {
    let mut graph = with_assigned_role();
    let domain = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 30)
        .unwrap()
        .group
        .as_mut()
        .unwrap();
    for slot in 1..64 {
        domain.nodes.push(node(
            200 + slot,
            &format!("filler_{slot}"),
            "node.fluid_role_source",
            None,
        ));
        domain
            .wires
            .push(wire(200 + slot, "role", 61, &format!("role_{slot}")));
    }
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect);
    resolve_target_instance(&target, &mut project)
        .unwrap()
        .graph = None;
    let before = serde_json::to_value(&project).unwrap();
    let mut duplicate = DuplicateSceneObjectCommand::new(target.clone(), vec![], 0, 0, graph);
    duplicate.execute(&mut project);
    assert!(!duplicate.was_applied());
    assert!(
        duplicate
            .rejection_reason()
            .unwrap()
            .contains("no free role ports")
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    let owner = project.graph_target_owner(&target).unwrap();
    assert_eq!((owner.graph_version, owner.graph_structure_version), (0, 0));
}

#[test]
fn scene_physics_object_fluid_tracking_undo_redo_and_stale_edits() {
    let graph = with_assigned_role();
    for remove in [false, true] {
        let (mut project, effect) = project_with_graph(graph.clone());
        let target = GraphTarget::Effect(effect.clone());
        resolve_target_instance(&target, &mut project)
            .unwrap()
            .graph = None;
        let mut command: Box<dyn Command> = if remove {
            Box::new(RemoveSceneObjectCommand::new(
                target.clone(),
                vec![],
                0,
                0,
                graph.clone(),
            ))
        } else {
            Box::new(DuplicateSceneObjectCommand::new(
                target.clone(),
                vec![],
                0,
                0,
                graph.clone(),
            ))
        };
        command.execute(&mut project);
        assert!(command.was_applied(), "{:?}", command.rejection_reason());
        let after = graph_of(&project, &effect).clone();
        for _ in 0..2 {
            command.undo(&mut project);
            assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
            command.execute(&mut project);
            assert!(command.was_applied());
            assert_eq!(graph_of(&project, &effect), &after);
        }
        let saved = serde_json::to_string(&project).unwrap();
        let reloaded: Project = serde_json::from_str(&saved).unwrap();
        assert_eq!(graph_of(&reloaded, &effect), &after);
        command.undo(&mut project);
        let owner = resolve_target_instance(&target, &mut project).unwrap();
        owner.graph = Some(graph.clone());
        let before = serde_json::to_value(&project).unwrap();
        let version = project.graph_target_owner(&target).unwrap().graph_version;
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert!(
            command
                .rejection_reason()
                .unwrap()
                .contains("changed since undo")
        );
        command.undo(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph_version,
            version
        );
    }
}

#[test]
fn scene_physics_object_fluid_modifier_local_undo_restores_owner() {
    let (mut project, target, _) = crate::commands::graph::test_support::modifier_draft_fixture();
    let mut local = with_assigned_role();
    let meta = local.preset_metadata.as_mut().unwrap();
    meta.params.push(ParamSpecDef {
        id: "11_pos_y".into(),
        name: "Y".into(),
        min: -10.0,
        max: 10.0,
        section: Some("Object — Transform".into()),
        ..Default::default()
    });
    meta.bindings.push(BindingDef {
        id: "11_pos_y".into(),
        label: "Y".into(),
        default_value: 0.0,
        target: BindingTarget::Node {
            node_id: NodeId::new("transform"),
            param: "pos_y".into(),
        },
        convert: Default::default(),
        user_added: false,
        scale: 1.0,
        offset: 0.0,
        default_mirrors_node_param: true,
    });
    let owner = project.graph_target_owner_mut(&target).unwrap();
    *target.graph_in_mut(owner.graph.as_mut().unwrap()).unwrap() = local;
    refresh_target_manifest(&mut project, &target);
    let owner = project.graph_target_owner_mut(&target).unwrap();
    let binding = owner.graph.as_mut().unwrap().preset_metadata.as_mut().unwrap().bindings.iter_mut()
        .find(|binding| matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "11_pos_y")).unwrap();
    binding.scale = 2.0;
    binding.offset = 0.5;
    let source_slot = binding.id.clone();
    owner.set_base_param(&source_slot, 1.5);
    let before = project.graph_target_owner(&target).unwrap().graph.clone();
    let mut duplicate =
        DuplicateSceneObjectCommand::new(target.clone(), vec![], 0, 0, before.clone().unwrap());
    duplicate.execute(&mut project);
    assert!(
        duplicate.was_applied(),
        "{:?}",
        duplicate.rejection_reason()
    );
    let duplicated = project.graph_target_owner(&target).unwrap().graph.clone();
    let duplicate_slot = duplicated.as_ref().unwrap().preset_metadata.as_ref().unwrap().bindings.iter()
        .find(|binding| matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "11_pos_y_duplicate"))
        .unwrap().id.clone();
    assert_eq!(
        project
            .graph_target_owner(&target)
            .unwrap()
            .get_base_param(&duplicate_slot),
        3.5,
        "a modifier copy preserves the resolved authored value through its new control"
    );
    let mut remove =
        RemoveSceneObjectCommand::new(target.clone(), vec![], 0, 0, before.clone().unwrap());
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    remove.undo(&mut project);
    assert_eq!(
        project.graph_target_owner(&target).unwrap().graph,
        duplicated
    );
    duplicate.undo(&mut project);
    assert_eq!(project.graph_target_owner(&target).unwrap().graph, before);
    let local = project.graph_for_target(&target, None).unwrap();
    let source = scene_fluid_role_assignments(local, 10).unwrap()[0]
        .source
        .clone();
    let mut remove_role =
        RemoveSceneFluidRoleCommand::new(target.clone(), source, before.clone().unwrap());
    remove_role.execute(&mut project);
    assert!(
        remove_role.was_applied(),
        "{:?}",
        remove_role.rejection_reason()
    );
    remove_role.undo(&mut project);
    assert_eq!(project.graph_target_owner(&target).unwrap().graph, before);
    // A sibling edit after undo must not be overwritten by the old owner snapshot.
    project
        .graph_target_owner_mut(&target)
        .unwrap()
        .graph
        .as_mut()
        .unwrap()
        .scene_modifiers[1]
        .graph
        .name = Some("Edited sibling".into());
    let edited = serde_json::to_value(&project).unwrap();
    remove_role.execute(&mut project);
    assert!(!remove_role.was_applied());
    assert!(
        remove_role
            .rejection_reason()
            .unwrap()
            .contains("changed since undo")
    );
    remove_role.undo(&mut project);
    assert_eq!(serde_json::to_value(&project).unwrap(), edited);
}

#[test]
fn scene_physics_object_fluid_queued_edits_reject_replaced_source() {
    let graph = with_assigned_role();
    let (mut project, effect) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(effect);
    let mut duplicate =
        DuplicateSceneObjectCommand::new(target.clone(), vec![], 0, 0, graph.clone());
    duplicate.execute(&mut project);
    assert!(duplicate.was_applied());
    let mut queued: Vec<Box<dyn Command>> = vec![
        Box::new(
            DuplicateSceneObjectCommand::new(target.clone(), vec![], 0, 0, graph.clone())
                .with_expected_source(NodeId::new("object_group")),
        ),
        Box::new(
            RemoveSceneObjectCommand::new(target.clone(), vec![], 0, 0, graph.clone())
                .with_expected_source(NodeId::new("object_group")),
        ),
    ];
    let mut remove = RemoveSceneObjectCommand::new(target.clone(), vec![], 0, 0, graph);
    remove.execute(&mut project);
    assert!(remove.was_applied());
    let before = serde_json::to_value(&project).unwrap();
    let version = project.graph_target_owner(&target).unwrap().graph_version;
    for command in &mut queued {
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert!(
            command
                .rejection_reason()
                .unwrap()
                .contains("selected object changed")
        );
        command.undo(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph_version,
            version
        );
    }
}
