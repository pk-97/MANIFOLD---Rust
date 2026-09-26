use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef, Transform};
use std::{borrow::Cow, cell::Cell};

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
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
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("pose"),
            ty: PortType::Transform,
            kind: PortKind::Input,
            required: true,
        }];
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
    }
}

fn runtime(height: f32, fused: bool) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.pose", || {
        Box::new(PoseObserver(EffectNodeType::new("test.pose")))
    });
    let def = serde_json::from_value(serde_json::json!({
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
    PresetRuntime::from_def_for_render(def, &registry, None, fused).unwrap()
}

fn frame(runtime: &mut PresetRuntime, seconds: f64) -> Transform {
    POSE.set(None);
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
                "id": id, "nodeId": name, "typeId": "node.gain"
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
