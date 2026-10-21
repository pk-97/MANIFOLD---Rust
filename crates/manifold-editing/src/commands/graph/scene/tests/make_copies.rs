//! Make Copies (copy_group_design.md sections 3 and 5): one scene object
//! becomes a copy group in one undo step, with refusals that carry reasons,
//! and Cmd+D on the group copies the whole group.
use super::*;
use manifold_core::physics_members::{Family, world_member};
use manifold_core::scene_slots::SceneSlot;

fn copies_params() -> Vec<NodeParamMetadata> {
    vec![
        scene_param_meta("copy_count", "Count"),
        scene_param_meta("copy_size_spacing", "Spacing"),
        scene_param_meta("copy_columns", "Columns"),
        scene_param_meta("copy_layout", "Layout"),
    ]
}

fn make_copies(project: &Project, target: &GraphTarget, graph: &EffectGraphDef, index: u32) -> Result<MakeCopyGroupCommand, manifold_core::scene_graph_edit::SceneGraphEditError> {
    let object = object_at(live_graph_of(project, target, graph), 0, index);
    MakeCopyGroupCommand::new(project, target.clone(), object, &body_params(), &[], &copies_params(), graph)
}

fn live_graph_of<'a>(project: &'a Project, target: &GraphTarget, default: &'a EffectGraphDef) -> &'a EffectGraphDef {
    project.graph_for_target(target, Some(default)).unwrap_or(default)
}

fn group_of(def: &EffectGraphDef) -> &GroupDef {
    def.nodes.iter().find_map(|node| node.group.as_deref()).expect("the import group")
}

fn param(node: &EffectGraphNode, name: &str) -> Option<SerializedParamValue> {
    node.params.get(name).cloned()
}

fn world_input(def: &EffectGraphDef, world: u32, port: &str) -> Option<EffectGraphWire> {
    def.wires.iter().find(|wire| wire.to_node == world && wire.to_port == port).cloned()
}

/// Every graph edit Make Copies makes lands as one undo step, and redo
/// reuses the prepared ids.
#[test]
fn make_copies_on_an_import_is_one_undo_step_and_redo_keeps_ids() {
    let graph = imported_group_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let before = project.graph_target_owner(&target).unwrap().clone();
    let mut command = make_copies(&project, &target, &graph, 0).expect("an import takes Make Copies");
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());

    let made = graph_of_generator(&project, &layer).clone();
    let world = made.nodes.iter().find(|node| node.type_id == "node.physics_world").expect("Make Copies brings the world");
    assert!(world_input(&made, world.id, "copies_0").is_some_and(|wire| wire.from_port == "body"), "the group joins the world as copy group 0");
    assert!(world_input(&made, world.id, "body_0").is_none(), "a copy group takes no body slot");
    let group = group_of(&made);
    let body = group.nodes.iter().find(|node| node.type_id == "node.rigid_body").expect("the copies body");
    let object = group.nodes.iter().find(|node| node.type_id == "node.scene_object").unwrap();
    let wired = |from_port: &str, to_port: &str| {
        group.wires.iter().any(|wire| wire.from_node == body.id && wire.from_port == from_port && wire.to_node == object.id && wire.to_port == to_port)
    };
    assert!(wired("instances", "instances"), "the object draws the body's instances");
    assert!(wired("active_count", "instance_count"), "and as many as the group makes");
    assert!(!wired("pose", "transform"), "the object no longer takes one pose");
    assert_eq!(param(body, "copy_count"), Some(SerializedParamValue::Float { value: 4.0 }));
    assert_eq!(param(body, "copy_layout"), Some(SerializedParamValue::Enum { value: 2 }), "a Line");
    assert_eq!(
        param(body, "copy_size_spacing"),
        Some(SerializedParamValue::Float { value: manifold_core::copies_body::SIZE_SPACING_DEFAULT })
    );
    let render = made.nodes.iter().find(|node| node.type_id == "node.render_scene").expect("the scene");
    use manifold_core::SceneNodeRefExt;
    let scene = SceneNodeRef::locate(&made, &render.node_id).expect("the scene is addressable");
    let models = manifold_core::scene_object_model::scene_object_models(&made, &scene).expect("models");
    let drawn = models.iter().find(|model| model.slot == Some(0)).expect("slot 0");
    assert_eq!(drawn.copy_group().map(|body| body.node.clone()), Some(body.node_id.clone()), "the model reads the copy group");
    assert!(!drawn.capabilities.make_copies);
    assert!(drawn.controls.iter().any(|control| control.node == body.node_id), "the body is on the object's card");

    let meta = made.preset_metadata.as_ref().unwrap();
    for name in ["copy_count", "copy_size_spacing", "copy_layout"] {
        assert!(
            meta.bindings.iter().any(|binding| matches!(&binding.target,
                BindingTarget::Node { node_id, param } if *node_id == body.node_id && param == name)),
            "{name} is a card row"
        );
    }

    command.undo(&mut project);
    let undone = project.graph_target_owner(&target).unwrap();
    assert_eq!(undone.graph, before.graph, "one undo takes the whole edit back");
    assert_eq!(undone.params, before.params);
    command.execute(&mut project);
    assert_eq!(graph_of_generator(&project, &layer), &made, "redo reuses the prepared ids");
}

/// A loose body already in the world (the shape the Physics Boxes preset
/// has before it is a group) moves from a body slot to a copy group.
#[test]
fn make_copies_moves_a_loose_physics_body_to_a_copy_group() {
    let graph = physics_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let mut command = make_copies(&project, &target, &graph, 0).expect("a physics object takes Make Copies");
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    let made = graph_of_generator(&project, &layer);
    let member = world_member(&made.nodes, &made.wires, 101).expect("the body is still in the world");
    assert_eq!((member.family, member.k), (Family::Copies, 0));
    assert!(!made.wires.iter().any(|wire| wire.to_node == 104 && wire.to_port == "transform"), "no pose into the object");
    assert!(made.wires.iter().any(|wire| wire.from_node == 101 && wire.from_port == "instances" && wire.to_node == 104));
    assert!(made.wires.iter().any(|wire| wire.from_node == 101 && wire.from_port == "active_count" && wire.to_node == 104));
}

/// Water, a compound import, a copy group and a full world refuse before
/// anything changes, each with its own reason.
#[test]
fn make_copies_refusals_carry_their_reasons() {
    // Water.
    let water: EffectGraphDef = serde_json::from_str(include_str!(
        "../../../../../../manifold-nodes/assets/generator-presets/WaterDamBreakGpuFlip.json"
    ))
    .expect("preset parses");
    let scene = water.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap();
    let water_object = water.nodes.iter().find(|node| node.node_id.as_str() == "water_family").unwrap().id;
    let water_slot = water
        .wires
        .iter()
        .find(|wire| wire.from_node == water_object && wire.to_node == scene.id)
        .and_then(|wire| wire.to_port.strip_prefix("object_")?.parse::<u32>().ok())
        .expect("the water's slot");
    let reason = make_copies_eligibility(&water, SceneSlot { render_id: scene.id, index: water_slot }).expect_err("water refuses");
    assert!(reason.starts_with("Water cannot make copies"), "{reason}");

    // A compound import.
    let compound = compound_imported_group_scene_graph();
    let reason = make_copies_eligibility(&compound, SceneSlot { render_id: 0, index: 0 }).expect_err("a compound refuses");
    assert_eq!(reason, "Copies take single-mesh objects; this import has 2 parts");

    // A copy group already.
    let graph = imported_group_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let mut command = make_copies(&project, &target, &graph, 0).unwrap();
    command.execute(&mut project);
    let made = graph_of_generator(&project, &layer).clone();
    let reason = make_copies_eligibility(&made, SceneSlot { render_id: 0, index: 0 }).expect_err("a copy group refuses");
    assert_eq!(reason, "This object is already a copy group");

    // No free copy-group input.
    let mut graph = builtin_scene_object_graph("node.cube_mesh");
    let world_id = 90;
    let mut world = EffectGraphNode { id: world_id, node_id: NodeId::new("full_world"), type_id: "node.physics_world".into(), ..graph.nodes[0].clone() };
    world.group = None;
    world.params.clear();
    graph.nodes.push(world);
    for k in 0..manifold_core::scene_surface::PHYSICS_WORLD_MAX_COPY_GROUPS {
        graph.wires.push(super::super::scene_build_wire(900 + k, "body", world_id, &format!("copies_{k}")));
    }
    let reason = make_copies_eligibility(&graph, SceneSlot { render_id: 0, index: 0 }).expect_err("a full world refuses");
    assert_eq!(reason, "The Physics World has no free copy-group input");
    let (project, layer) = project_with_generator_graph(graph.clone());
    let refused = make_copies(&project, &GraphTarget::Generator(layer), &graph, 0).expect_err("the command refuses too");
    assert_eq!(refused.message, "The Physics World has no free copy-group input");
}

/// A scale that is not one number cannot reach the world, so Make Copies
/// names it instead of copying it.
#[test]
fn make_copies_refuses_a_non_uniform_scale() {
    let mut graph = imported_group_scene_graph();
    let group = graph.nodes.iter_mut().find_map(|node| node.group.as_deref_mut()).unwrap();
    let transform = group.nodes.iter_mut().find(|node| node.type_id == "node.transform_3d").unwrap();
    transform.params.insert("scale_x".into(), SerializedParamValue::Float { value: 2.0 });
    let reason = make_copies_eligibility(&graph, SceneSlot { render_id: 0, index: 0 }).expect_err("a stretched object refuses");
    assert_eq!(reason, "Make Copies needs one scale on all three axes");
}

/// Cmd+D on a copy group copies the whole group into the next free
/// copy-group input, 0.5 m on along X, in one undo step.
#[test]
fn duplicating_a_copy_group_takes_the_next_copy_group_input() {
    let graph = imported_group_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let mut command = make_copies(&project, &target, &graph, 0).unwrap();
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    let made = graph_of_generator(&project, &layer).clone();
    let before = project.graph_target_owner(&target).unwrap().clone();

    let mut copy = duplicate_one(&project, target.clone(), vec![], 0, 0, &graph).expect("a copy group duplicates");
    copy.execute(&mut project);
    assert!(copy.was_applied(), "{:?}", copy.rejection_reason());
    let doubled = graph_of_generator(&project, &layer).clone();
    let world = doubled.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
    assert!(world_input(&doubled, world.id, "copies_0").is_some());
    assert!(world_input(&doubled, world.id, "copies_1").is_some(), "the duplicate is copy group 1");
    assert_eq!(manifold_core::scene_variadic::WORLD_COPY_GROUPS.count(world), 2);
    let groups: Vec<&EffectGraphNode> = doubled.nodes.iter().filter(|node| node.type_id == GROUP_TYPE_ID).collect();
    assert_eq!(groups.len(), 2);
    let moved = groups[1].group.as_ref().unwrap().nodes.iter().find(|node| node.type_id == "node.transform_3d").unwrap();
    assert_eq!(param(moved, "pos_x"), Some(SerializedParamValue::Float { value: 0.5 }), "0.5 m along X");
    let body = groups[1].group.as_ref().unwrap().nodes.iter().find(|node| node.type_id == "node.rigid_body").unwrap();
    assert_eq!(param(body, "copy_count"), Some(SerializedParamValue::Float { value: 4.0 }), "the copy group keeps its count");
    assert_ne!(made, doubled);

    copy.undo(&mut project);
    assert_eq!(project.graph_target_owner(&target).unwrap().graph, before.graph, "one undo");
}
