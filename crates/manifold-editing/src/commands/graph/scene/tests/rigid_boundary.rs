//! PHYSICS_ENGINE_BOUNDARY_DESIGN.md P6a: rigid enable and duplicate commit
//! through the shared scene transaction, one undo step each.
use super::*;

fn bodies(graph: &EffectGraphDef) -> Vec<&EffectGraphNode> {
    let grouped = graph.nodes.iter().filter_map(|node| node.group.as_deref()).flat_map(|group| group.nodes.iter());
    graph.nodes.iter().chain(grouped).filter(|node| node.type_id == "node.rigid_body").collect()
}

fn worlds(graph: &EffectGraphDef) -> Vec<u32> {
    graph.nodes.iter().filter(|node| node.type_id == "node.physics_world").map(|node| node.id).collect()
}

/// The body slots the world's inputs fill, ascending.
fn filled_slots(graph: &EffectGraphDef, world: u32) -> Vec<u32> {
    let mut slots: Vec<u32> = graph
        .wires
        .iter()
        .filter(|wire| wire.to_node == world)
        .filter_map(|wire| match manifold_core::physics_members::member_input(&wire.to_port) {
            Some((manifold_core::physics_members::Family::Body, slot)) => Some(slot),
            _ => None,
        })
        .collect();
    slots.sort_unstable();
    slots
}

fn enable(project: &Project, target: &GraphTarget, default: &EffectGraphDef) -> EnableSceneObjectPhysicsCommand {
    enable_cmd(project, target.clone(), 0, 0, &body_params(), &[], default)
        .expect("the imported model takes physics")
}

/// An imported model with physics on, in a generator layer.
fn enabled_photoscan() -> (Project, LayerId, GraphTarget, EffectGraphDef) {
    let graph = imported_group_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let mut command = enable(&project, &target, &graph);
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    (project, layer, target, graph)
}

#[test]
fn physics_boundary_rigid_enable() {
    let graph = imported_group_scene_graph();
    let (mut project, layer) = project_with_generator_graph(graph.clone());
    let target = GraphTarget::Generator(layer.clone());
    let before = project.graph_target_owner(&target).unwrap().clone();
    let mut command = enable(&project, &target, &graph);
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    let enabled = graph_of_generator(&project, &layer).clone();
    let world = worlds(&enabled);
    assert_eq!(world.len(), 1, "the first body brings the shared world");
    assert_eq!(filled_slots(&enabled, world[0]), vec![0]);
    assert_eq!(bodies(&enabled).len(), 1);
    let body = bodies(&enabled)[0].node_id.clone();
    assert!(
        enabled.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| matches!(&binding.target,
            BindingTarget::Node { node_id, param } if *node_id == body && param == "density")),
        "the body's controls are on the card"
    );

    command.undo(&mut project);
    let undone = project.graph_target_owner(&target).unwrap();
    assert_eq!(undone.graph, before.graph, "one undo removes the body and the world");
    assert_eq!(undone.params, before.params);
    command.execute(&mut project);
    assert_eq!(graph_of_generator(&project, &layer), &enabled, "redo reuses the prepared ids");

    // A second enable on the same object refuses before submission.
    let refused = enable_cmd(&project, target, 0, 0, &body_params(), &[], &graph)
        .expect_err("physics is already on");
    assert_eq!(refused.message, "Selected object already has standard physics enabled");
}

#[test]
fn physics_boundary_rigid_shared_world() {
    let (mut project, layer, target, graph) = enabled_photoscan();
    let copy = duplicate_copies(&project, target.clone(), vec![], 0, 0, 1, &graph);
    let mut copy = copy.expect("a physics object copies");
    copy.execute(&mut project);
    assert!(copy.was_applied(), "{:?}", copy.rejection_reason());
    let def = graph_of_generator(&project, &layer).clone();
    let world = worlds(&def);
    assert_eq!(world.len(), 1, "every body shares one world; no world per object");
    assert_eq!(filled_slots(&def, world[0]), vec![0, 1]);
    let bodies = bodies(&def);
    assert_eq!(bodies.len(), 2);
    assert_ne!(bodies[0].node_id, bodies[1].node_id, "each copy owns its body");

    // Switching one body off keeps the world, the other body and both slots.
    let off = bodies[1].node_id.clone();
    let group = def
        .nodes
        .iter()
        .find(|node| node.group.as_ref().is_some_and(|group| group.nodes.iter().any(|inner| inner.node_id == off)))
        .unwrap()
        .id;
    let body_doc = def.nodes.iter().find(|node| node.id == group).unwrap().group.as_ref().unwrap().nodes.iter()
        .find(|node| node.node_id == off).unwrap().id;
    let mut switch_off = SetGraphNodeParamCommand::new(
        target,
        body_doc,
        "enabled".into(),
        SerializedParamValue::Bool { value: false },
        graph,
    )
    .with_scope(vec![group]);
    switch_off.execute(&mut project);
    let after = graph_of_generator(&project, &layer);
    assert_eq!(worlds(after), world);
    assert_eq!(filled_slots(after, world[0]), vec![0, 1], "a body switched off keeps its slot and settings");
    assert_eq!(self::bodies(after).len(), 2);
}

#[test]
fn physics_boundary_rigid_duplicate_paused() {
    let (mut project, layer, target, graph) = enabled_photoscan();
    // Pause the simulation, as a performer does before copying.
    let world_id = worlds(graph_of_generator(&project, &layer))[0];
    let mut pause = SetGraphNodeParamCommand::new(
        target.clone(),
        world_id,
        "speed".into(),
        SerializedParamValue::Float { value: 0.0 },
        graph.clone(),
    );
    pause.execute(&mut project);
    let paused = graph_of_generator(&project, &layer).clone();
    let world_before = paused.nodes.iter().find(|node| node.id == world_id).unwrap().clone();

    let mut copy = duplicate_copies(&project, target.clone(), vec![], 0, 0, 1, &graph).unwrap();
    copy.execute(&mut project);
    assert!(copy.was_applied(), "{:?}", copy.rejection_reason());
    let copied = graph_of_generator(&project, &layer).clone();
    assert_eq!(worlds(&copied), vec![world_id], "no new world");
    assert_eq!(bodies(&copied).len(), 2, "exactly one new body");
    assert_eq!(
        copied.nodes.iter().find(|node| node.id == world_id).unwrap(),
        &world_before,
        "the world keeps its pause and its settings"
    );
    assert_eq!(filled_slots(&copied, world_id), vec![0, 1]);

    copy.undo(&mut project);
    assert_eq!(graph_of_generator(&project, &layer), &paused);
    copy.execute(&mut project);
    assert_eq!(graph_of_generator(&project, &layer), &copied, "redo reuses the prepared ids");
}

#[test]
fn physics_boundary_rigid_duplicate_count() {
    let (mut project, layer, target, graph) = enabled_photoscan();
    let before = project.graph_target_owner(&target).unwrap().clone();
    let mut copies = duplicate_copies(&project, target.clone(), vec![], 0, 0, 3, &graph).unwrap();
    copies.execute(&mut project);
    assert!(copies.was_applied(), "{:?}", copies.rejection_reason());
    let def = graph_of_generator(&project, &layer).clone();
    let world = worlds(&def);
    assert_eq!(world.len(), 1, "N copies share one world");
    assert_eq!(filled_slots(&def, world[0]), vec![0, 1, 2, 3], "one new body per copy, next free slots");
    assert_eq!(bodies(&def).len(), 4);
    assert_eq!(
        def.nodes.iter().find(|node| node.id == 0).unwrap().params.get("objects"),
        Some(&SerializedParamValue::Float { value: 4.0 })
    );
    let groups: Vec<_> = def.nodes.iter().filter(|node| node.type_id == GROUP_TYPE_ID).collect();
    let names: Vec<_> = groups.iter().map(|group| group.handle.as_deref().unwrap()).collect();
    assert_eq!(names, ["Imported", "Imported 2", "Imported 3", "Imported 4"]);
    for (copy, group) in groups.iter().enumerate().skip(1) {
        let body = group.group.as_ref().unwrap();
        let x = body.nodes.iter().find(|node| node.type_id == "node.transform_3d").unwrap().params.get("pos_x");
        assert_eq!(x, Some(&SerializedParamValue::Float { value: 0.5 * copy as f32 }), "each copy sits 0.5 m on from the last");
        let source = body.nodes.iter().find(|node| node.type_id == "node.gltf_mesh_source").unwrap();
        assert!(
            def.preset_metadata.as_ref().unwrap().string_bindings.iter().any(|binding| matches!(&binding.target,
                BindingTarget::Node { node_id, .. } if *node_id == source.node_id)),
            "every copy keeps the Model File"
        );
    }

    copies.undo(&mut project);
    let undone = project.graph_target_owner(&target).unwrap();
    assert_eq!(undone.graph, before.graph, "one undo removes every copy");
    assert_eq!(undone.params, before.params);
    copies.execute(&mut project);
    assert_eq!(graph_of_generator(&project, &layer), &def, "redo reuses the prepared ids");
}

#[test]
fn physics_boundary_rigid_duplicate_count_refuses_a_full_world() {
    let (mut project, layer, target, graph) = enabled_photoscan();
    // Fill every body slot but two with bodies from elsewhere.
    let world = worlds(graph_of_generator(&project, &layer))[0];
    let host = project.graph_target_owner_mut(&target).unwrap();
    let def = host.graph.as_mut().unwrap();
    for slot in 3..manifold_core::scene_surface::PHYSICS_WORLD_MAX_BODIES {
        def.wires.push(super::super::scene_build_wire(999, "body", world, &format!("body_{slot}")));
    }
    let full = def.clone();
    let refused = duplicate_copies(&project, target.clone(), vec![], 0, 0, 3, &graph)
        .expect_err("three copies cannot fit in two free slots");
    assert_eq!(refused.message, "The Physics World has 2 free body slots; 3 copies need 3");
    assert_eq!(graph_of_generator(&project, &layer), &full);
    let mut two = duplicate_copies(&project, target, vec![], 0, 0, 2, &graph).unwrap();
    two.execute(&mut project);
    assert!(two.was_applied(), "{:?}", two.rejection_reason());
    let result = graph_of_generator(&project, &layer);
    assert_eq!(filled_slots(result, world).len(), manifold_core::scene_surface::PHYSICS_WORLD_MAX_BODIES as usize);
    let world_node = result.nodes.iter().find(|node| node.id == world).unwrap();
    assert_eq!(
        manifold_core::physics_body_slots::world_bodies(world_node),
        manifold_core::scene_surface::PHYSICS_WORLD_MAX_BODIES,
        "the copies grew the world to its last slot"
    );
}

/// A copy carries its source's drivers but not its Ableton mappings: one Live
/// knob keeps driving the one object it was mapped to, as Ableton's own
/// duplicate leaves MIDI and key mappings behind.
#[test]
fn physics_boundary_rigid_duplicate_leaves_ableton_mappings_on_the_source() {
    use manifold_core::ableton_mapping::{
        AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
    };
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use std::borrow::Cow;

    let (mut project, layer, target, graph) = enabled_photoscan();
    let density_bindings = |project: &Project| -> Vec<String> {
        graph_of_generator(project, &layer).preset_metadata.as_ref().unwrap().bindings.iter()
            .filter(|binding| matches!(&binding.target, BindingTarget::Node { param, .. } if param == "density"))
            .map(|binding| binding.id.clone())
            .collect()
    };
    let [source] = density_bindings(&project).try_into().expect("one bound Density");
    let host = project.graph_target_owner_mut(&target).unwrap();
    host.drivers = Some(vec![ParameterDriver {
        param_id: Cow::Owned(source.clone()),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.25,
        base_value: 0.4,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        free_period_beats: None,
        frame_aligned: false,
        legacy_param_index: None,
        is_paused_by_user: false,
    }]);
    host.ableton_mappings = Some(vec![AbletonParamMapping {
        param_id: Cow::Owned(source.clone()),
        address: AbletonMacroAddress {
            track_id: 1,
            device_id: 2,
            param_id: 3,
            device_identity: AbletonDeviceIdentity { device_class_name: "Rack".into() },
            track_name: "Track".into(),
            device_name: "Device".into(),
            macro_name: "Macro".into(),
        },
        range_min: 0.0,
        range_max: 1.0,
        inverted: false,
        legacy_param_index: None,
        last_value: 0.5,
        status: AbletonMappingStatus::Active,
    }]);

    let mut copy = duplicate_copies(&project, target.clone(), vec![], 0, 0, 1, &graph).unwrap();
    copy.execute(&mut project);
    assert!(copy.was_applied(), "{:?}", copy.rejection_reason());
    let copied = density_bindings(&project).into_iter().find(|id| *id != source).expect("the copy binds its Density");
    let host = project.graph_target_owner(&target).unwrap();
    let driven: Vec<&str> = host.drivers.as_ref().unwrap().iter().map(|driver| driver.param_id.as_ref()).collect();
    assert_eq!(driven, [source.as_str(), copied.as_str()], "the copy carries the driver");
    let mapped: Vec<&str> = host.ableton_mappings.as_ref().unwrap().iter().map(|mapping| mapping.param_id.as_ref()).collect();
    assert_eq!(mapped, [source.as_str()], "the Ableton mapping stays on the source");
}
