use super::*;
use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind};
use crate::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef, scene::transform::Transform};
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
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
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
        POSE_READY.set(ctx.inputs.slot("pose").is_some_and(|slot| ctx.inputs.slot_content_ready(slot)));
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
    registry.register("test.pose", || {
        Box::new(PoseObserver(EffectNodeType::new("test.pose")))
    });
    let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Native physics carry",
        "nodes": [
            {"id":0,"nodeId":"input","typeId":"system.generator_input"},
            {"id":1,"nodeId":"start","typeId":"node.transform_3d","params":{
                "pos_y":{"type":"Float","value":height}
            }},
            {"id":2,"nodeId":"body","typeId":"node.rigid_body"},
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
                "id":7,"nodeId":"field","typeId":"node.uniform_vector_field","params":{
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
            serde_json::json!({"id":8,"nodeId":"peer_body","typeId":"node.rigid_body"}),
            serde_json::json!({"id":9,"nodeId":"peer_start","typeId":"node.transform_3d","params":{
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
    for (actual, expected) in [(actual, expected), (PEER_POSE.get().unwrap(), expected_peer)] {
        for axis in 0..3 {
            assert!((actual.pos[axis] - expected.pos[axis]).abs() < 1e-4,
                "offline gap changed the native trajectory: {actual:?} vs {expected:?}");
        }
    }
}

#[test]
fn offline_history_drain_keeps_capped_preview_prefix_before_long_gap() {
    use crate::water::physics::PhysicsStepScope;
    let accepted_prefix = 2.0 / 60.0;
    let mut expected_runtime = runtime_with_field(50.0, false, true);
    frame(&mut expected_runtime, 0.0);
    let expected_prefix = frame(&mut expected_runtime, accepted_prefix);
    let expected = frame(&mut expected_runtime, accepted_prefix + (3.0 - 0.9));

    let mut jumped = runtime_with_field(50.0, false, true);
    {
        let _preview = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        frame(&mut jumped, 0.0);
        let behind = frame(&mut jumped, 0.9);
        assert_eq!(behind, expected_prefix, "live accepts exactly two fixed intervals even at zero budget");
    }
    let actual = frame(&mut jumped, 3.0);
    for axis in 0..3 {
        assert!((actual.pos[axis] - expected.pos[axis]).abs() < 1e-4,
            "offline drain changed the accepted live prefix: {actual:?} vs {expected:?}");
    }
}

#[test]
fn physics_carry_rejects_setup_changes_and_output_resize() {
    let mut prior = runtime(5.0, false);
    frame(&mut prior, 0.0);
    let fallen = frame(&mut prior, 0.5);
    let mut changed = runtime(8.0, false);
    changed.carry_generator_state_from(&mut prior);
    assert!(changed.last_physics_frame_time.is_none());
    assert_eq!(frame(&mut changed, 0.5).pos[1], 8.0);
    let mut resized = runtime(5.0, false);
    resized.width += 1;
    resized.carry_generator_state_from(&mut prior);
    assert!(resized.last_physics_frame_time.is_none());
    assert_ne!(frame(&mut resized, 0.5), fallen);
}
