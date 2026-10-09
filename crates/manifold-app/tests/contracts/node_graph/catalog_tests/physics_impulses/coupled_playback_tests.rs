//! Exercise real paired native playback through the ordinary graph executor.
use super::*;
use manifold_node_engine::water::fluid::CoupledRigidFrame;
use manifold_node_engine::scene::impulse::RigidImpulseTargets;
use manifold_node_engine::water::physics::PhysicsStepScope;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::types::LayerType;
use manifold_core::{GraphTarget, NodeId, PresetTypeId};

fn authored_shared_world_fixture() -> EffectGraphDef {
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::AddSceneFluidCommand;

    let baseline = fixture();
    let mut project = manifold_core::project::Project::default();
    let preset = PresetTypeId::new("SharedWorldPlayback");
    let layer_index =
        project
            .timeline
            .add_layer("Shared World Playback", LayerType::Generator, preset);
    project.timeline.layers[layer_index]
        .gen_params_or_init()
        .graph = Some(baseline.clone());
    let target = GraphTarget::Generator(project.timeline.layers[layer_index].layer_id.clone());
    let render_id = baseline
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("fixture render scene")
        .id;
    let mut add = AddSceneFluidCommand::new(
        target.clone(),
        render_id,
        manifold_nodes::testkit::reference_fixtures::cpu_flip_metadata(),
        manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.transform_3d"),
        manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.pbr_material"),
        manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.scene_object"),
        manifold_editing::commands::graph::flip_scene_fluid_template(),
        baseline,
    )
    .with_role_metadata(manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type(
        "node.fluid_role_source",
    ))
    .with_world_metadata(manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type(
        "node.physics_world",
    ));
    add.execute(&mut project);
    assert!(
        add.was_applied(),
        "add fluid rejected: {:?}",
        add.rejection_reason()
    );
    project
        .graph_for_target(&target, None)
        .expect("authored graph after add fluid")
        .clone()
}

fn generated_fluid_id(def: &EffectGraphDef) -> NodeId {
    def.nodes
        .iter()
        .find_map(|node| {
            node.group.as_deref()?.nodes.iter().find_map(|child| {
                (child.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID).then(|| child.node_id.clone())
            })
        })
        .expect("generated fluid node")
}

fn shared_control_id(def: &EffectGraphDef, name: &str) -> String {
    let suffix = format!("_{name}");
    def.preset_metadata
        .as_ref()
        .expect("shared metadata")
        .bindings
        .iter()
        .find(|binding| binding.id.ends_with(&suffix))
        .map(|binding| binding.id.clone())
        .unwrap_or_else(|| panic!("missing shared binding {name}"))
}

fn shared_control_ids(def: &EffectGraphDef) -> [String; 5] {
    [
        shared_control_id(def, "gravity_x"),
        shared_control_id(def, "gravity_y"),
        shared_control_id(def, "gravity_z"),
        shared_control_id(def, "speed"),
        shared_control_id(def, "reset"),
    ]
}

fn manifest_for(def: &EffectGraphDef) -> ParamManifest {
    ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .expect("shared metadata")
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    )
}

fn set_control(manifest: &mut ParamManifest, id: &str, value: f32) {
    manifest
        .get_mut(id)
        .unwrap_or_else(|| panic!("missing control {id}"))
        .value = value;
}

fn assert_saved_shared_routes(def: &EffectGraphDef, ids: &[String; 5]) {
    let metadata = def.preset_metadata.as_ref().expect("shared metadata");
    for id in ids {
        let routes = metadata
            .bindings
            .iter()
            .filter(|binding| binding.id == *id)
            .collect::<Vec<_>>();
        assert_eq!(routes.len(), 1, "shared id {id} must remain one route");
        assert!(matches!(
            &routes[0].target,
            BindingTarget::Node { param, .. } if param == "value"
        ));
    }
}

#[track_caller]
fn paired_frame_for(runtime: &PresetRuntime, fluid_id: &NodeId) -> CoupledRigidFrame {
    let fluid = runtime
        .graph
        .instance_by_node_id(fluid_id)
        .expect("generated fluid runtime node");
    let node = runtime.graph.get_node(fluid).expect("generated fluid node");
    let node = node::get(node.node.as_ref()).expect("native fluid fixture");
    assert!(
        node.coupled_rigid_frame().is_some(),
        "completed paired native frame: {:?}; {:?}",
        node.fluid_domain_snapshot(),
        runtime.scene_viewport_errors()
    );
    node.coupled_rigid_frame().unwrap().clone()
}

fn execute_authored_frame(runtime: &mut PresetRuntime, seconds: f64) {
    // Add Fluid prepares source geometry asynchronously. Observe readiness at
    // the same timestamp instead of assuming a fixed number of warmup frames.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        runtime.execute_frame(time(seconds));
        assert!(
            runtime.scene_viewport_errors().is_empty(),
            "{:?}",
            runtime.scene_viewport_errors()
        );
        if !runtime.warmup_pending() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "authored source preparation timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn coupled_fixture() -> EffectGraphDef {
    let mut def = fixture();
    for node in &mut def.nodes {
        if node.node_id.as_str() == "pose_a" {
            node.params.insert(
                "pos_y".into(),
                manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
            );
        }
        if node.node_id.as_str() == "body_a" {
            node.params.insert(
                "density".into(),
                manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1000.0 },
            );
        }
    }
    // Preserve every existing rigid scene member; add the liquid as another
    // object using the same authoring path as an ordinary scene.
    def.nodes.extend([
        serde_json::from_value(serde_json::json!({
            "id":14,"nodeId":"fluid","typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
            "params":{
                "resolution":{"type":"Float","value":12.0},
                "gravity":{"type":"Float","value":0.0},
                "fill_height":{"type":"Float","value":0.5},
                "emission":{"type":"Float","value":0.0}
            }
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "id":15,"nodeId":"liquid","typeId":"node.scene_object"
        }))
        .unwrap(),
    ]);
    def.wires.extend([
        serde_json::from_value(serde_json::json!({
            "fromNode":14,"fromPort":"vertices","toNode":15,"toPort":"vertices"
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "fromNode":15,"fromPort":"object","toNode":10,"toPort":"object_3"
        }))
        .unwrap(),
    ]);
    def.nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "scene")
        .unwrap()
        .params
        .insert(
            "objects".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Int { value: 4 },
        );
    def
}

fn paired_frame(runtime: &PresetRuntime) -> CoupledRigidFrame {
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    node::get(runtime.graph.get_node(fluid).unwrap().node.as_ref())
        .expect("native fluid fixture")
        .coupled_rigid_frame()
        .expect("completed paired native frame")
        .clone()
}

fn observed_fluid_time(runtime: &PresetRuntime, fluid_id: &NodeId, transport: f64) -> Seconds {
    let fluid = runtime.graph.instance_by_node_id(fluid_id).unwrap();
    node::get(runtime.graph.get_node(fluid).unwrap().node.as_ref())
        .expect("native fluid fixture")
        .physics_impulse_stamp(Seconds(transport), 0)
        .expect("accepted fluid clock observation").time
}

fn assert_visible_pair(runtime: &PresetRuntime, frame: &CoupledRigidFrame) {
    assert_eq!(
        POSITIONS.get(),
        [frame.poses[0].pos[0], frame.poses[1].pos[0]],
        "downstream consumers must see the same rigid frame accepted by liquid"
    );
    let world = runtime
        .graph
        .instance_by_node_id(&NodeId::new("world"))
        .unwrap();
    assert!(
        node::get(runtime.graph.get_node(world).unwrap().node.as_ref())
            .expect("native world fixture")
            .physics_impulse_epoch()
            .is_none(),
        "the rigid publisher must not own another native simulation"
    );
}

#[test]
fn coupled_graph_preview_holds_pair_then_offline_drains_without_double_advancement() {
    let mut runtime = runtime(&coupled_fixture());
    let initial;
    {
        let _preview = PhysicsStepScope::for_render(false);
        // Initialize in live mode, as the content pipeline does. Observe the
        // asynchronous initial reply without advancing transport or changing
        // the clock mode before its first accepted interval.
        let fluid = runtime.graph.instance_by_node_id(&NodeId::new("fluid")).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            runtime.execute_frame(time(0.0));
            assert!(runtime.scene_viewport_errors().is_empty(), "{:?}", runtime.scene_viewport_errors());
            if node::get(runtime.graph.get_node(fluid).unwrap().node.as_ref())
                .expect("native fluid fixture").coupled_rigid_frame().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "initial paired frame timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        initial = paired_frame(&runtime);
        assert_eq!(initial.stamp.tick, 0);
        assert_visible_pair(&runtime, &initial);
        runtime.execute_frame(time(3.0 * DT));
        let held = paired_frame(&runtime);
        // Nonblocking submit cannot accept the newly submitted reply in this
        // call, even if the worker finishes immediately on another core.
        assert_eq!(held.stamp, initial.stamp);
        assert_eq!(held.poses, initial.poses);
        assert_visible_pair(&runtime, &held);
    }
    runtime.execute_frame(time(3.0 * DT));
    let caught_up = paired_frame(&runtime);
    // The live frame accepted two fixed intervals and discarded the third.
    // Offline draining completes that pair without recovering discarded time.
    assert_eq!(caught_up.stamp.tick, 2);
    assert_eq!(observed_fluid_time(&runtime, &NodeId::new("fluid"), 3.0 * DT), Seconds(2.0 * DT));
    assert_eq!(caught_up.stamp.epoch, initial.stamp.epoch);
    assert!(caught_up.poses[0].pos[0] > initial.poses[0].pos[0]);
    assert_visible_pair(&runtime, &caught_up);
    runtime.execute_frame(time(3.0 * DT));
    assert_eq!(
        paired_frame(&runtime).poses,
        caught_up.poses,
        "paused frame adds no tick"
    );
    assert_visible_pair(&runtime, &caught_up);
}

#[test]
fn coupled_graph_either_reset_restarts_both_visible_participants() {
    let mut runtime = runtime(&coupled_fixture());
    runtime.execute_frame(time(0.0));
    runtime.execute_frame(time(DT));
    let before = paired_frame(&runtime);
    for (node, value) in [("world", 1.0), ("fluid", 1.0), ("world", 2.0)] {
        let previous = paired_frame(&runtime).stamp;
        edit(&mut runtime, node, "reset", value);
        runtime.execute_frame(time(DT));
        let reset = paired_frame(&runtime);
        assert_eq!(reset.stamp.epoch, previous.epoch + 1);
        assert_eq!(reset.stamp.tick, 0);
        assert_eq!(reset.poses[0].pos[0], 0.0);
        assert_visible_pair(&runtime, &reset);
    }
    assert!(before.poses[0].pos[0] > 0.0);
    let previous = paired_frame(&runtime).stamp;
    edit(&mut runtime, "world", "reset", 3.0);
    edit(&mut runtime, "fluid", "reset", 2.0);
    runtime.execute_frame(time(DT));
    let reset = paired_frame(&runtime);
    assert_eq!(reset.stamp.epoch, previous.epoch + 1);
    assert_eq!(reset.stamp.tick, 0);
    assert_visible_pair(&runtime, &reset);
}

#[test]
fn coupled_graph_merges_shared_impulses_and_preserves_single_material_selections() {
    let def = coupled_fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let body = RigidImpulseTargets {
        bodies: 1,
        copies: false,
    };
    for (sequence, selection, target) in [
        (1, vec!["part_a"], ImpulseTarget::Rigid(body)),
        (2, vec!["liquid"], ImpulseTarget::Fluid),
        (
            3,
            vec!["part_a", "part_a_2", "liquid"],
            ImpulseTarget::FluidAndRigid(body),
        ),
    ] {
        let mut binding = prepare(&runtime, &def, &selection);
        assert_eq!(binding.test_recipient_count(), 1);
        assert_eq!(binding.test_recipient_id(0).as_str(), "fluid");
        assert_eq!(binding.test_recipient_target(0), target);
        let mut captured = binding.new_capture();
        runtime
            .water()
            .capture_scene_impulse_at_source(&mut binding, &mut captured, time(0.0), sequence)
            .unwrap();
        runtime.water().deliver_scene_impulse(&mut captured).unwrap();
        runtime.water().deliver_scene_impulse(&mut captured).unwrap();
        assert_eq!(captured.scheduled_ticks().count(), 1);
    }
    runtime.execute_frame(time(DT));
    let mut receipts = Vec::new();
    runtime.water().drain_scene_impulses(|id, event| {
        assert_eq!(id.as_str(), "fluid");
        assert_eq!(event.applied.tick, 0);
        receipts.push((event.source.sequence, event.value.target));
    });
    assert_eq!(
        receipts,
        vec![
            (1, ImpulseTarget::Rigid(body)),
            (2, ImpulseTarget::Fluid),
            (3, ImpulseTarget::FluidAndRigid(body)),
        ]
    );
    assert_visible_pair(&runtime, &paired_frame(&runtime));
}

#[test]
fn authored_add_fluid_shared_controls_play_back_after_reload() {
    let authored = authored_shared_world_fixture();
    let fluid_id = generated_fluid_id(&authored);
    let ids = shared_control_ids(&authored);
    assert_saved_shared_routes(&authored, &ids);

    let saved = serde_json::to_string(&authored).expect("save authored graph");
    let restored: EffectGraphDef = serde_json::from_str(&saved).expect("reload authored graph");
    assert_saved_shared_routes(&restored, &ids);

    let mut runtime = runtime(&restored);
    let mut manifest = manifest_for(&restored);
    runtime.apply_param_values(&manifest);
    let _offline = PhysicsStepScope::for_render(true);
    execute_authored_frame(&mut runtime, 0.0);
    let initial = paired_frame_for(&runtime, &fluid_id);

    // The shared source value reaches both the root World and the coupled
    // liquid through their ordinary graph routes.
    set_control(&mut manifest, &ids[0], 3.0);
    set_control(&mut manifest, &ids[1], 0.0);
    set_control(&mut manifest, &ids[2], -4.0);
    set_control(&mut manifest, &ids[3], 2.0);
    runtime.apply_param_values(&manifest);
    let BindingTarget::Node {
        node_id: speed_source,
        param: speed_param,
    } = &restored
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| binding.id == ids[3])
        .unwrap()
        .target
    else {
        panic!("shared speed source");
    };
    let speed_source = runtime.graph.instance_by_node_id(speed_source).unwrap();
    assert_eq!(
        runtime
            .graph
            .get_node(speed_source)
            .unwrap()
            .params
            .get(speed_param.as_str()),
        Some(&ParamValue::Float(2.0)),
        "shared speed binding must reach its authored source"
    );
    // Observe the edit at its actual boundary. Historical sampling correctly
    // retains the previous speed until this observation, including offline.
    execute_authored_frame(&mut runtime, 0.0);
    execute_authored_frame(&mut runtime, DT);
    assert_eq!(
        runtime
            .graph
            .get_node(speed_source)
            .unwrap()
            .params
            .get(speed_param.as_str()),
        Some(&ParamValue::Float(2.0)),
        "frame execution must retain the shared speed binding"
    );
    let fast = paired_frame_for(&runtime, &fluid_id);
    // Export accepts one project interval, advanced at the authored speed 2.
    assert_eq!(fast.stamp.tick, 1);
    assert_eq!(observed_fluid_time(&runtime, &fluid_id, DT), Seconds(2.0 * DT));
    assert_eq!(fast.stamp.epoch, initial.stamp.epoch);
    assert!(fast.poses[0].pos[0] > initial.poses[0].pos[0]);
    assert!(
        fast.poses[0].pos[2] < initial.poses[0].pos[2],
        "shared Z gravity moves the real body; the fixture field only drives X"
    );
    assert_visible_pair(&runtime, &fast);

    set_control(&mut manifest, &ids[3], 0.0);
    runtime.apply_param_values(&manifest);
    execute_authored_frame(&mut runtime, DT);
    execute_authored_frame(&mut runtime, 3.0 * DT);
    let held = paired_frame_for(&runtime, &fluid_id);
    assert_eq!(
        held.stamp, fast.stamp,
        "shared zero speed holds both participants"
    );
    assert_eq!(held.poses, fast.poses);

    set_control(&mut manifest, &ids[3], 1.0);
    set_control(&mut manifest, &ids[4], 1.0);
    runtime.apply_param_values(&manifest);
    execute_authored_frame(&mut runtime, 3.0 * DT);
    let reset = paired_frame_for(&runtime, &fluid_id);
    assert_eq!(reset.stamp.epoch, fast.stamp.epoch + 1);
    assert_eq!(reset.stamp.tick, 0);
    assert_eq!(reset.poses[0].pos, initial.poses[0].pos);
    assert_visible_pair(&runtime, &reset);

    execute_authored_frame(&mut runtime, 4.0 * DT);
    let held_reset = paired_frame_for(&runtime, &fluid_id);
    assert_eq!(held_reset.stamp.epoch, reset.stamp.epoch);
    assert!(held_reset.stamp.tick > reset.stamp.tick);
}
