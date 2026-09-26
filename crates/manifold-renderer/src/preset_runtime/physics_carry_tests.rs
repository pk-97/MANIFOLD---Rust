use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef, Transform};
use std::{borrow::Cow, cell::Cell};

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
    static PEER_POSE: Cell<Option<Transform>> = const { Cell::new(None) };
}

struct PoseObserver(EffectNodeType);
impl EffectNode for PoseObserver {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
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
    POSE.set(None);
    runtime.execute_frame(FrameTime {
        seconds: Seconds::ZERO,
        beats: Beats::ZERO,
        delta: Seconds(1.0 / 30.0),
        frame_count: 0,
    });
    assert!(
        POSE.get().is_none(),
        "a missing recipient must not silently advance the world"
    );
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

#[test]
fn physics_carry_matches_owners_across_actual_fused_topology() {
    let mut def: EffectGraphDef = serde_json::from_str(include_str!(
        "../../assets/generator-presets/WaterBasin.json"
    ))
    .unwrap();
    // A fusible image segment after the scene changes the execution plan,
    // while the native simulation and its CPU ancestry stay authored nodes.
    for (id, name) in [(400, "gain_a"), (401, "gain_b")] {
        def.nodes.push(
            serde_json::from_value(serde_json::json!({
                "id": id, "nodeId": name, "typeId": "node.exposure"
            }))
            .unwrap(),
        );
    }
    def.wires.retain(|wire| wire.to_node != 31);
    for (from_node, from_port, to_node, to_port) in [
        (30, "color", 400, "in"),
        (400, "out", 401, "in"),
        (401, "out", 31, "in"),
    ] {
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
    }
    let registry = PrimitiveRegistry::with_builtin();
    let mut prior =
        PresetRuntime::from_def_for_render(def.clone(), &registry, None, false).unwrap();
    let mut fused = PresetRuntime::from_def_for_render(def, &registry, None, true).unwrap();
    assert!(
        fused.graph.nodes().count() < prior.graph.nodes().count(),
        "fixture must really fuse"
    );
    let owner = |runtime: &PresetRuntime| {
        let id = runtime
            .graph
            .instance_by_node_id(&NodeId::new("fluid_surface"))
            .unwrap();
        &*runtime.graph.get_node(id).unwrap().node as *const dyn EffectNode as *const ()
    };
    let native_owner = owner(&prior);
    assert_ne!(owner(&fused), native_owner);
    prior.last_physics_frame_time = Some(FrameTime {
        seconds: Seconds(0.5),
        beats: Beats(1.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: 15,
    });
    fused.carry_generator_state_from(&mut prior);
    assert_eq!(owner(&fused), native_owner);
    assert_eq!(fused.last_physics_frame_time.unwrap().seconds, Seconds(0.5));
}
