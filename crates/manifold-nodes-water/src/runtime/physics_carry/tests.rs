use super::*;
use crate::runtime::WaterRuntimeExt;
use manifold_core::Beats;
use manifold_core::Seconds;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind};
use manifold_node_engine::{
    exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext,
    exec::effect_node::EffectNodeType, parameters::ParamDef, scene::transform::Transform,
};
use std::{borrow::Cow, cell::Cell};

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
    static PEER_POSE: Cell<Option<Transform>> = const { Cell::new(None) };
    static POSE_READY: Cell<bool> = const { Cell::new(false) };
}

struct PoseObserver(EffectNodeType);
impl EffectNode for PoseObserver {
    fn is_liveness_root(&self) -> bool {
        true
    }
    // Observes readiness, so it must run while the pose is pending.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 2] = [
            NodePort {
                name: Cow::Borrowed("pose"),
                ty: PortType::Transform,
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: Cow::Borrowed("peer_pose"),
                ty: PortType::Transform,
                kind: PortKind::Input,
                required: false,
            },
        ];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        POSE_READY.set(
            ctx.inputs
                .slot("pose")
                .is_some_and(|slot| ctx.inputs.slot_content_ready(slot)),
        );
        POSE.set(ctx.inputs.transform("pose"));
        PEER_POSE.set(ctx.inputs.transform("peer_pose"));
    }
}

fn runtime(height: f32, fused: bool) -> PresetRuntime {
    runtime_with_field(height, fused, false)
}

fn runtime_with_field(height: f32, fused: bool, field: bool) -> PresetRuntime {
    runtime_with_field_port(height, fused, field.then_some("acceleration_field"))
}

fn runtime_with_field_port(height: f32, fused: bool, field_port: Option<&str>) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.pose", || {
        Box::new(PoseObserver(EffectNodeType::new("test.pose")))
    });
    let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Native physics carry",
        "nodes": [
            {"id":0,"nodeId":"input","typeId":"system.generator_input"},
            {"id":1,"nodeId":"start","typeId":"test.physics_transform","params":{
                "pos_y":{"type":"Float","value":height}
            }},
            {"id":2,"nodeId":"body","typeId":"test.physics_body"},
            {"id":3,"nodeId":"world","typeId":"node.physics_world"},
            {"id":4,"nodeId":"observe","typeId":"test.pose"},
            {"id":5,"nodeId":"source","typeId":"system.source"},
            {"id":6,"nodeId":"output","typeId":"system.final_output"}
        ],
        "wires": [
            {"fromNode":1,"fromPort":"transform","toNode":2,"toPort":"transform"},
            {"fromNode":2,"fromPort":"body","toNode":3,"toPort":"body_0"},
            {"fromNode":3,"fromPort":"pose_0","toNode":4,"toPort":"pose"},
            {"fromNode":5,"fromPort":"out","toNode":6,"toPort":"in"}
        ]
    }))
    .unwrap();
    if let Some(field_port) = field_port {
        def.nodes.push(
            serde_json::from_value(serde_json::json!({
                "id":7,"nodeId":"field","typeId":"test.physics_field","params":{
                    "x":{"type":"Float","value":4.0},
                    "y":{"type":"Float","value":0.0}
                }
            }))
            .unwrap(),
        );
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node: 7,
                from_port: "out".into(),
                to_node: 3,
                to_port: field_port.into(),
            });
    }
    if field_port == Some("body_acceleration_0") {
        for node in [
            serde_json::json!({"id":8,"nodeId":"peer_body","typeId":"test.physics_body"}),
            serde_json::json!({"id":9,"nodeId":"peer_start","typeId":"test.physics_transform","params":{
                "pos_x":{"type":"Float","value":10.0},
                "pos_y":{"type":"Float","value":height}
            }}),
        ] {
            def.nodes.push(serde_json::from_value(node).unwrap());
        }
        for (from_node, from_port, to_node, to_port) in [
            (9, "transform", 8, "transform"),
            (8, "body", 3, "body_1"),
            (3, "pose_1", 4, "peer_pose"),
        ] {
            def.wires
                .push(manifold_core::effect_graph_def::EffectGraphWire {
                    from_node,
                    from_port: from_port.into(),
                    to_node,
                    to_port: to_port.into(),
                });
        }
    }
    PresetRuntime::from_def_for_render(def, &registry, None, fused).unwrap()
}

fn frame(runtime: &mut PresetRuntime, seconds: f64) -> Transform {
    POSE.set(None);
    PEER_POSE.set(None);
    runtime.execute_frame(FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: (seconds * 30.0) as i64,
    });
    POSE.get().expect("native world publishes pose")
}

#[test]
fn physics_carry_preserves_native_rigid_trajectory_and_paused_pose() {
    let mut reference = runtime(5.0, false);
    let mut edited = runtime(5.0, false);
    for time in [0.0, 0.033, 0.12, 0.24] {
        assert_eq!(frame(&mut reference, time), frame(&mut edited, time));
    }
    let paused = frame(&mut edited, 0.24);
    assert!(paused.pos[1] < 5.0, "fixture must be falling");
    for fused in [true, false, true] {
        let mut rebuilt = runtime(5.0, fused);
        rebuilt.carry_generator_state_from(&mut edited);
        edited = rebuilt;
        assert_eq!(
            frame(&mut edited, 0.24),
            paused,
            "editor rebuild reset or stepped the body"
        );
    }
    for time in [0.273, 0.35, 0.5] {
        let mut rebuilt = runtime(5.0, false);
        rebuilt.carry_generator_state_from(&mut edited);
        edited = rebuilt;
        assert_eq!(
            frame(&mut reference, time),
            frame(&mut edited, time),
            "trajectory changed at {time}"
        );
    }
}

#[test]
fn physics_shared_field_graph_is_frame_rate_independent_and_survives_rebuild() {
    let mut reference = runtime_with_field(5.0, false, true);
    let mut expected = Transform::default();
    for tick in 0..=30 {
        expected = frame(&mut reference, tick as f64 / 60.0);
    }
    assert!(expected.pos[0] > 0.4, "the graph field must reach Box3D");
    for fps in [24, 30] {
        let mut edited = runtime_with_field(5.0, false, true);
        let mut actual = Transform::default();
        for tick in 0..=fps / 2 {
            if tick > 0 {
                let mut rebuilt = runtime_with_field(5.0, tick % 2 == 0, true);
                rebuilt.carry_generator_state_from(&mut edited);
                edited = rebuilt;
            }
            actual = frame(&mut edited, tick as f64 / fps as f64);
        }
        for axis in 0..3 {
            assert!(
                (actual.pos[axis] - expected.pos[axis]).abs() < 1e-5,
                "{fps} FPS/rebuild changed trajectory: {actual:?} vs {expected:?}"
            );
        }
    }
}

#[test]
fn physics_targeted_field_graph_preserves_recipients_across_rebuild_and_frame_rates() {
    let mut reference = runtime_with_field_port(5.0, false, Some("body_acceleration_0"));
    let mut expected = Transform::default();
    for tick in 0..=30 {
        expected = frame(&mut reference, tick as f64 / 60.0);
    }
    assert!(
        expected.pos[0] > 0.4,
        "targeted field must reach the selected body"
    );
    let peer = PEER_POSE.get().expect("second body must be live");
    assert_eq!(
        peer.pos[0], 10.0,
        "unselected body must receive no horizontal force"
    );
    assert!(peer.pos[1] < 5.0, "unselected body must still simulate");
    for fps in [24, 30] {
        let mut edited = runtime_with_field_port(5.0, false, Some("body_acceleration_0"));
        for tick in 0..=fps / 2 {
            if tick > 0 {
                let mut rebuilt =
                    runtime_with_field_port(5.0, tick % 2 == 0, Some("body_acceleration_0"));
                rebuilt.carry_generator_state_from(&mut edited);
                edited = rebuilt;
            }
            let actual = frame(&mut edited, tick as f64 / fps as f64);
            assert_eq!(PEER_POSE.get().unwrap().pos[0], 10.0);
            if tick == fps / 2 {
                for axis in 0..3 {
                    assert!(
                        (actual.pos[axis] - expected.pos[axis]).abs() < 1e-5,
                        "{fps} FPS/rebuild changed selected body: {actual:?} vs {expected:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn physics_targeted_field_graph_without_recipient_stays_pending() {
    let mut runtime = runtime_with_field_port(5.0, false, Some("body_acceleration_9"));
    POSE_READY.set(true);
    runtime.execute_frame(FrameTime {
        seconds: Seconds::ZERO,
        beats: Beats::ZERO,
        delta: Seconds(1.0 / 30.0),
        frame_count: 0,
    });
    assert!(
        !POSE_READY.get(),
        "a missing recipient must leave the output pending, even if the slot retains an old/default pose"
    );
}

#[test]
fn offline_history_drain_long_gap_matches_native_rigid_frame_sequence() {
    let mut reference = runtime_with_field_port(50.0, false, Some("body_acceleration_0"));
    let mut expected = Transform::default();
    for tick in 0..=180 {
        expected = frame(&mut reference, tick as f64 / 60.0);
    }
    let expected_peer = PEER_POSE.get().unwrap();
    let mut jumped = runtime_with_field_port(50.0, false, Some("body_acceleration_0"));
    frame(&mut jumped, 0.0);
    let actual = frame(&mut jumped, 3.0);
    assert!(POSE_READY.get());
    assert!(actual.pos[0] > 10.0, "fixture must have accelerated");
    for (actual, expected) in [
        (actual, expected),
        (PEER_POSE.get().unwrap(), expected_peer),
    ] {
        for axis in 0..3 {
            assert!(
                (actual.pos[axis] - expected.pos[axis]).abs() < 1e-4,
                "offline gap changed the native trajectory: {actual:?} vs {expected:?}"
            );
        }
    }
}

#[test]
fn offline_history_drain_keeps_capped_preview_prefix_before_long_gap() {
    use manifold_water_rigid::physics::SimStep;
    let accepted_prefix = 2.0 / 60.0;
    let mut expected_runtime = runtime_with_field(50.0, false, true);
    frame(&mut expected_runtime, 0.0);
    let expected_prefix = frame(&mut expected_runtime, accepted_prefix);
    let expected = frame(&mut expected_runtime, accepted_prefix + (3.0 - 0.9));

    let mut jumped = runtime_with_field(50.0, false, true);
    {
        let tick = manifold_core::Seconds(manifold_physics::clock::TICK);
        jumped.set_sim_step(SimStep::live(tick).with_preview_budget(std::time::Duration::ZERO));
        frame(&mut jumped, 0.0);
        let behind = frame(&mut jumped, 0.9);
        assert_eq!(
            behind, expected_prefix,
            "live accepts exactly two fixed intervals even at zero budget"
        );
        jumped.set_sim_step(SimStep::default());
    }
    let actual = frame(&mut jumped, 3.0);
    for axis in 0..3 {
        assert!(
            (actual.pos[axis] - expected.pos[axis]).abs() < 1e-4,
            "offline drain changed the accepted live prefix: {actual:?} vs {expected:?}"
        );
    }
}

#[test]
fn physics_carry_applies_setup_changes_in_place_and_rejects_output_resize() {
    let mut prior = runtime(5.0, false);
    frame(&mut prior, 0.0);
    let fallen = frame(&mut prior, 0.5);
    let mut changed = runtime(8.0, false);
    changed.carry_generator_state_from(&mut prior);
    assert!(
        changed.water_ref().water.last_frame_time.is_some(),
        "the world carries over"
    );
    assert_eq!(
        frame(&mut changed, 0.5).pos[1],
        8.0,
        "the edited start pose lands as a teleport"
    );
    let mut resized = runtime(5.0, false);
    resized.set_dimensions_for_test(resized.width_for_test() + 1, resized.height_for_test());
    resized.carry_generator_state_from(&mut prior);
    assert!(resized.water_ref().water.last_frame_time.is_none());
    assert_ne!(frame(&mut resized, 0.5), fallen);
}

/// Speed and a push that both move with transport time, as in-graph curves.
struct MovingControls(EffectNodeType, [NodeOutput; 2]);
impl EffectNode for MovingControls {
    fn is_pure(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("clock"),
            ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.1
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(manifold_node_engine::parameters::ParamValue::Float(clock)) =
            ctx.inputs.scalar("clock")
        else {
            panic!("moving controls clock")
        };
        ctx.outputs.set_scalar(
            "speed",
            manifold_node_engine::parameters::ParamValue::Float(1.0 + 0.6 * (clock * 5.0).sin()),
        );
        let field = manifold_physics::FieldValue::uniform([6.0 * (clock * 7.0).sin(), 0.0, 0.0])
            .expect("finite push");
        ctx.outputs.set_cpu_value("field", field);
    }
}

fn moving_runtime() -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.pose", || {
        Box::new(PoseObserver(EffectNodeType::new("test.pose")))
    });
    registry.register("test.moving_controls", || {
        let output = |name, ty| NodePort {
            name: Cow::Borrowed(name),
            ty,
            kind: PortKind::Output,
            required: false,
        };
        Box::new(MovingControls(
            EffectNodeType::new("test.moving_controls"),
            [
                output(
                    "speed",
                    PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
                ),
                output("field", PortType::VectorField),
            ],
        ))
    });
    let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Moving native physics",
        "nodes": [
            {"id":0,"nodeId":"input","typeId":"system.generator_input"},
            {"id":1,"nodeId":"start","typeId":"test.physics_transform","params":{
                "pos_y":{"type":"Float","value":50.0}
            }},
            {"id":2,"nodeId":"body","typeId":"test.physics_body"},
            {"id":3,"nodeId":"world","typeId":"node.physics_world"},
            {"id":4,"nodeId":"observe","typeId":"test.pose"},
            {"id":5,"nodeId":"source","typeId":"system.source"},
            {"id":6,"nodeId":"output","typeId":"system.final_output"},
            {"id":7,"nodeId":"controls","typeId":"test.moving_controls"}
        ],
        "wires": [
            {"fromNode":1,"fromPort":"transform","toNode":2,"toPort":"transform"},
            {"fromNode":2,"fromPort":"body","toNode":3,"toPort":"body_0"},
            {"fromNode":0,"fromPort":"time","toNode":7,"toPort":"clock"},
            {"fromNode":7,"fromPort":"speed","toNode":3,"toPort":"speed"},
            {"fromNode":7,"fromPort":"field","toNode":3,"toPort":"acceleration_field"},
            {"fromNode":3,"fromPort":"pose_0","toNode":4,"toPort":"pose"},
            {"fromNode":5,"fromPort":"out","toNode":6,"toPort":"in"}
        ]
    }))
    .unwrap();
    PresetRuntime::from_def_for_render(def, &registry, None, false).unwrap()
}

/// The pose after one second at `fps`, its bits.
fn moving_pose(fps: u32, offline: bool) -> [u32; 3] {
    use manifold_water_rigid::physics::SimStep;
    let tick = manifold_core::Seconds(manifold_physics::clock::TICK);
    let mut runtime = moving_runtime();
    runtime.set_sim_step(if offline {
        SimStep::export(tick)
    } else {
        SimStep::live(tick)
    });
    let mut pose = Transform::default();
    for index in 0..=fps {
        let seconds = f64::from(index) / f64::from(fps);
        POSE.set(None);
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: Beats(seconds * 2.0),
            delta: Seconds(if index == 0 {
                0.0
            } else {
                1.0 / f64::from(fps)
            }),
            frame_count: i64::from(index),
        });
        pose = POSE.get().expect("native world publishes pose");
    }
    pose.pos.map(f32::to_bits)
}

/// The standalone rigid world takes Speed and its inputs at its own tick
/// starts, never between frame-end samples: a Speed curve and a push that
/// move with time land the body on the same bits at every rate, live and
/// exported.
#[test]
fn rigid_world_moving_speed_and_push_match_at_30_60_120_144_fps_live_and_export() {
    let reference = moving_pose(60, true);
    assert!(
        f32::from_bits(reference[0]).abs() > 0.05,
        "the push must move the body: {reference:?}"
    );
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            assert_eq!(
                moving_pose(fps, offline),
                reference,
                "{fps} fps, offline {offline}"
            );
        }
    }
}

/// The falling-body graph, plus a second body wired into `body_1` when
/// `peer`: the edit Enable Physics, Duplicate and Paste make. `sized` sizes
/// the world to exactly the bodies it holds, as a world past its room is.
fn runtime_with_peer(peer: bool, sized: bool) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.pose", || {
        Box::new(PoseObserver(EffectNodeType::new("test.pose")))
    });
    let mut nodes = vec![
        serde_json::json!({"id":0,"nodeId":"input","typeId":"system.generator_input"}),
        serde_json::json!({"id":1,"nodeId":"start","typeId":"test.physics_transform","params":{"pos_y":{"type":"Float","value":5.0}}}),
        serde_json::json!({"id":2,"nodeId":"body","typeId":"test.physics_body"}),
        if sized {
            serde_json::json!({"id":3,"nodeId":"world","typeId":"node.physics_world","params":{"bodies":{"type":"Int","value":1 + i32::from(peer)}}})
        } else {
            serde_json::json!({"id":3,"nodeId":"world","typeId":"node.physics_world"})
        },
        serde_json::json!({"id":4,"nodeId":"observe","typeId":"test.pose"}),
        serde_json::json!({"id":5,"nodeId":"source","typeId":"system.source"}),
        serde_json::json!({"id":6,"nodeId":"output","typeId":"system.final_output"}),
    ];
    let mut wires = vec![
        serde_json::json!({"fromNode":1,"fromPort":"transform","toNode":2,"toPort":"transform"}),
        serde_json::json!({"fromNode":2,"fromPort":"body","toNode":3,"toPort":"body_0"}),
        serde_json::json!({"fromNode":3,"fromPort":"pose_0","toNode":4,"toPort":"pose"}),
        serde_json::json!({"fromNode":5,"fromPort":"out","toNode":6,"toPort":"in"}),
    ];
    if peer {
        nodes.push(serde_json::json!({"id":8,"nodeId":"peer_body","typeId":"test.physics_body"}));
        nodes.push(serde_json::json!({"id":9,"nodeId":"peer_start","typeId":"test.physics_transform","params":{
            "pos_x":{"type":"Float","value":10.0},"pos_y":{"type":"Float","value":5.0}}}));
        wires.push(serde_json::json!({"fromNode":9,"fromPort":"transform","toNode":8,"toPort":"transform"}));
        wires
            .push(serde_json::json!({"fromNode":8,"fromPort":"body","toNode":3,"toPort":"body_1"}));
        wires.push(
            serde_json::json!({"fromNode":3,"fromPort":"pose_1","toNode":4,"toPort":"peer_pose"}),
        );
    }
    let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Native physics carry", "nodes": nodes, "wires": wires
    }))
    .unwrap();
    PresetRuntime::from_def_for_render(def, &registry, None, false).unwrap()
}

/// Adding a body is a structural edit, so the generator rebuilds. The world
/// carries over: the body already falling keeps its trajectory and the new one
/// joins it.
#[test]
fn physics_carry_survives_a_body_being_added() {
    let mut reference = runtime_with_peer(false, false);
    let mut edited = runtime_with_peer(false, false);
    for time in [0.0, 0.033, 0.12, 0.24] {
        assert_eq!(frame(&mut reference, time), frame(&mut edited, time));
    }
    let paused = frame(&mut edited, 0.24);
    assert!(paused.pos[1] < 5.0, "fixture must be falling");
    let mut rebuilt = runtime_with_peer(true, false);
    rebuilt.carry_generator_state_from(&mut edited);
    assert_eq!(
        frame(&mut rebuilt, 0.24),
        paused,
        "adding a body reset or stepped the first one"
    );
    assert!(PEER_POSE.get().is_some(), "the added body is in the world");
    for time in [0.273, 0.35, 0.5] {
        assert_eq!(
            frame(&mut reference, time),
            frame(&mut rebuilt, time),
            "trajectory changed at {time}"
        );
    }

    // A world with no free port grows one for the new body. The carried world
    // takes the new size, so the new body is read and falls instead of
    // hanging where it was authored.
    let mut full = runtime_with_peer(false, true);
    for time in [0.0, 0.033, 0.12] {
        frame(&mut full, time);
    }
    let mut grown = runtime_with_peer(true, true);
    grown.carry_generator_state_from(&mut full);
    frame(&mut grown, 0.24);
    let joined = PEER_POSE
        .get()
        .expect("the grown world reads the added body");
    frame(&mut grown, 0.5);
    let later = PEER_POSE
        .get()
        .expect("the grown world reads the added body");
    assert!(
        later.pos[1] < joined.pos[1],
        "the added body hangs: {joined:?} then {later:?}"
    );
}
