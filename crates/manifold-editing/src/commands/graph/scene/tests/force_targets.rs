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
    let mut duplicate = duplicate_object(&project, 
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

    let mut remove = remove_object(&project, 
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
    let mut enable = enable_physics(&project, 
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
fn scene_force_targets_group_rename_preserves_identity() {
    let (graph, world, field) = imported_with_force();
    let (mut project, fx) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(fx.clone());
    let mut rename = rename_object(&project, 
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
    let field = connect_field(&mut graph, world, "copies_acceleration_0");
    let global = connect_field(&mut graph, world, "acceleration_field");
    let (mut project, fx) = project_with_graph(graph.clone());
    let mut remove = remove_object(&project, 
        GraphTarget::Effect(fx.clone()),
        vec![],
        30,
        1,
        graph.clone(),
    );
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    let result = graph_of(&project, &fx);
    assert!(!has_target(result, field, world, "copies_acceleration_0"));
    assert!(has_target(result, global, world, "acceleration_field"));
    assert!(result.nodes.iter().any(|node| node.id == field));
    remove.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);
}
