use super::*;

fn connect_field(graph: &mut EffectGraphDef, world_id: u32, port: &str) -> u32 {
    let id = super::super::max_node_id_over(&graph.nodes) + 1;
    graph.nodes.push(super::super::scene_build_node(
        id,
        "node.uniform_vector_field",
        Some(format!("Force {id}")),
        BTreeMap::new(),
    ));
    graph
        .wires
        .push(super::super::scene_build_wire(id, "out", world_id, port));
    id
}

fn has_target(graph: &EffectGraphDef, source: u32, world: u32, port: &str) -> bool {
    graph
        .wires
        .iter()
        .any(|wire| wire.from_node == source && wire.to_node == world && wire.to_port == port)
}

#[test]
fn scene_force_targets_duplicate_preserves_shared_source_and_roundtrips() {
    let mut graph = physics_scene_graph();
    let field = connect_field(&mut graph, 40, "body_acceleration_0");
    let global = connect_field(&mut graph, 40, "acceleration_field");
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut duplicate = DuplicateSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        graph.clone(),
    );
    duplicate.execute(&mut project);
    assert!(
        duplicate.was_applied(),
        "{:?}",
        duplicate.rejection_reason()
    );
    let duplicated = graph_of(&project, &fx).clone();
    assert!(has_target(&duplicated, field, 40, "body_acceleration_0"));
    assert!(has_target(&duplicated, field, 40, "body_acceleration_1"));
    assert!(has_target(&duplicated, global, 40, "acceleration_field"));
    assert_eq!(
        duplicated
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.uniform_vector_field")
            .count(),
        2
    );
    let loaded: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&duplicated).unwrap()).unwrap();
    assert_eq!(loaded, duplicated);
    duplicate.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
    duplicate.execute(&mut project);
    assert_eq!(graph_of(&project, &fx), &duplicated);

    let mut remove = RemoveSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        duplicated.clone(),
    );
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    let remaining = graph_of(&project, &fx);
    assert!(!has_target(remaining, field, 40, "body_acceleration_0"));
    assert!(has_target(remaining, field, 40, "body_acceleration_1"));
    assert!(has_target(remaining, global, 40, "acceleration_field"));
    assert!(remaining.nodes.iter().any(|n| n.id == field));
    assert_eq!(
        super::super::first_free_physics_body_slot(&remaining.wires, 40),
        Some(0)
    );
    remove.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &duplicated);
}

fn imported_with_force() -> (EffectGraphDef, u32, u32) {
    let graph = imported_group_scene_graph();
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut enable = EnableSceneObjectPhysicsCommand::new(
        GraphTarget::Effect(fx.clone()),
        0,
        0,
        body_params(),
        graph,
    );
    enable.execute(&mut project);
    assert!(enable.was_applied(), "{:?}", enable.rejection_reason());
    let mut graph = graph_of(&project, &fx).clone();
    let world = graph
        .nodes
        .iter()
        .find(|node| node.type_id == "node.physics_world")
        .unwrap()
        .id;
    let field = connect_field(&mut graph, world, "body_acceleration_0");
    (graph, world, field)
}

#[test]
fn scene_force_targets_group_rename_disable_and_reenable_preserve_identity() {
    let (graph, world, field) = imported_with_force();
    let (mut project, fx) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(fx.clone());
    let mut rename = RenameSceneObjectCommand::new(
        target.clone(),
        vec![],
        10,
        "Renamed Recipient".into(),
        graph.clone(),
    );
    rename.execute(&mut project);
    assert!(rename.was_applied());
    assert!(has_target(
        graph_of(&project, &fx),
        field,
        world,
        "body_acceleration_0"
    ));
    rename.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);

    let mut disable = DisableSceneObjectPhysicsCommand::new(target.clone(), 0, 0, graph.clone());
    disable.execute(&mut project);
    assert!(disable.was_applied(), "{:?}", disable.rejection_reason());
    let disabled = graph_of(&project, &fx).clone();
    assert!(!has_target(&disabled, field, world, "body_acceleration_0"));
    assert!(disabled.nodes.iter().any(|node| node.id == field));
    disable.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
    disable.execute(&mut project);
    let mut enable = EnableSceneObjectPhysicsCommand::new(target, 0, 0, body_params(), disabled);
    enable.execute(&mut project);
    assert!(enable.was_applied(), "{:?}", enable.rejection_reason());
    assert!(!has_target(
        graph_of(&project, &fx),
        field,
        world,
        "body_acceleration_0"
    ));
}

#[test]
fn scene_force_targets_split_inherits_acceleration_for_each_piece_and_undoes() {
    let (graph, world, field) = imported_with_force();
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut split = SplitSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        0,
        0,
        body_params(),
        graph.clone(),
    );
    split.execute(&mut project);
    assert!(split.was_applied(), "{:?}", split.rejection_reason());
    let split_graph = graph_of(&project, &fx);
    for slot in 0..8 {
        assert!(has_target(
            split_graph,
            field,
            world,
            &format!("body_acceleration_{slot}")
        ));
    }
    assert_eq!(
        split_graph
            .wires
            .iter()
            .filter(|wire| wire.from_node == field)
            .count(),
        8
    );
    split.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
}

#[test]
fn scene_force_targets_reserve_slots_with_dangling_field_or_pose_wires() {
    let mut graph = physics_scene_graph();
    connect_field(&mut graph, 40, "body_acceleration_1");
    graph.wires.push(super::super::scene_build_wire(
        40,
        "pose_2",
        999,
        "transform",
    ));
    assert_eq!(
        super::super::first_free_physics_body_slot(&graph.wires, 40),
        Some(3)
    );
}

#[test]
fn scene_force_targets_split_remaps_a_field_exported_by_the_split_group() {
    let (mut graph, world, field) = imported_with_force();
    let index = graph
        .nodes
        .iter()
        .position(|node| node.id == field)
        .unwrap();
    let field_node = graph.nodes.remove(index);
    let group = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == 10)
        .unwrap()
        .group
        .as_deref_mut()
        .unwrap();
    let output = group
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .unwrap()
        .id;
    group.nodes.push(field_node);
    group.interface.outputs.push(InterfacePortDef {
        name: "force".into(),
        port_type: "VectorField".into(),
    });
    group.wires.push(super::super::scene_build_wire(
        field, "out", output, "force",
    ));
    let wire = graph
        .wires
        .iter_mut()
        .find(|wire| wire.from_node == field)
        .unwrap();
    wire.from_node = 10;
    wire.from_port = "force".into();
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut split = SplitSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        0,
        0,
        body_params(),
        graph.clone(),
    );
    split.execute(&mut project);
    assert!(split.was_applied(), "{:?}", split.rejection_reason());
    let result = graph_of(&project, &fx);
    assert!(!result.nodes.iter().any(|node| node.id == 10));
    for slot in 0..8 {
        let body = result
            .wires
            .iter()
            .find(|wire| wire.to_node == world && wire.to_port == format!("body_{slot}"))
            .unwrap();
        assert!(has_target(
            result,
            body.from_node,
            world,
            &format!("body_acceleration_{slot}")
        ));
        let group = result
            .nodes
            .iter()
            .find(|node| node.id == body.from_node)
            .unwrap()
            .group
            .as_deref()
            .unwrap();
        assert!(
            group
                .interface
                .outputs
                .iter()
                .any(|port| port.name == "force")
        );
    }
    split.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
}

#[test]
fn scene_force_targets_copies_remove_only_their_field_and_undoes() {
    let mut graph: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-nodes/assets/generator-presets/PhysicsBoxes.json"
    )))
    .unwrap();
    let world = graph
        .nodes
        .iter()
        .find(|node| node.type_id == "node.physics_world")
        .unwrap()
        .id;
    let field = connect_field(&mut graph, world, "copies_acceleration");
    let global = connect_field(&mut graph, world, "acceleration_field");
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut remove = RemoveSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        30,
        1,
        graph.clone(),
    );
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    let result = graph_of(&project, &fx);
    assert!(!has_target(result, field, world, "copies_acceleration"));
    assert!(has_target(result, global, world, "acceleration_field"));
    assert!(result.nodes.iter().any(|node| node.id == field));
    remove.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
}
