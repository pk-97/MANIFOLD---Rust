use super::*;
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use std::{borrow::Cow, cell::RefCell};

#[derive(Debug)]
struct Observation {
    time: FrameTime,
    values: [f32; 5],
    draining: bool,
}

thread_local! {
    static OBSERVATIONS: RefCell<Vec<Observation>> = const { RefCell::new(Vec::new()) };
}

struct ObservedPhysics(EffectNodeType);

impl EffectNode for ObservedPhysics {
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
        const fn input(name: &'static str) -> NodeInput {
            NodePort {
                name: Cow::Borrowed(name),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }
        }
        static INPUTS: [NodeInput; 5] = [
            input("value"),
            input("clock"),
            input("beat"),
            input("trigger"),
            input("lfo"),
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
        let values = ["value", "clock", "beat", "trigger", "lfo"].map(|name| {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(value)) => value,
                value => panic!("missing observed scalar {name}: {value:?}"),
            }
        });
        OBSERVATIONS.with_borrow_mut(|observations| {
            observations.push(Observation {
                time: ctx.time,
                values,
                draining: crate::node_graph::physics::history_drain_requested(),
            })
        });
    }
}

fn runtime() -> PresetRuntime {
    OBSERVATIONS.with_borrow_mut(Vec::clear);
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("node.physics_world", || {
        Box::new(ObservedPhysics(EffectNodeType::new("node.physics_world")))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Observed physics controls",
        "nodes": [
            {"id": 0, "nodeId": "input", "typeId": "system.generator_input"},
            {"id": 1, "nodeId": "value", "typeId": "node.value"},
            {"id": 2, "nodeId": "lfo", "typeId": "node.lfo", "params": {
                "rate_mode": {"type": "Enum", "value": 1},
                "angular_rate": {"type": "Float", "value": 12.0},
                "min": {"type": "Float", "value": -1.0},
                "max": {"type": "Float", "value": 1.0}
            }},
            {"id": 3, "nodeId": "physics", "typeId": "node.physics_world"},
            {"id": 4, "nodeId": "source", "typeId": "system.source"},
            {"id": 5, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "time", "toNode": 3, "toPort": "clock"},
            {"fromNode": 0, "fromPort": "beat", "toNode": 3, "toPort": "beat"},
            {"fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "trigger"},
            {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "value"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "lfo"},
            {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in"}
        ]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

fn frame(runtime: &mut PresetRuntime, seconds: f64, value: f32, triggers: f32) -> Vec<Observation> {
    let time = FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(
            runtime
                .last_physics_frame_time
                .map_or(0.0, |previous| seconds - previous.seconds.0),
        ),
        frame_count: 0,
    };
    runtime.set_frame_context(FrameContextInputs {
        time: seconds as f32,
        beat: time.beats.0 as f32,
        aspect: 1.0,
        trigger_count: triggers,
        anim_progress: 0.0,
        output_width: 16.0,
        output_height: 16.0,
    });
    let node = runtime
        .graph
        .instance_by_node_id(&NodeId::new("value"))
        .unwrap();
    runtime
        .graph
        .set_param(node, "value", ParamValue::Float(value))
        .unwrap();
    runtime.execute_frame(time);
    OBSERVATIONS.with_borrow_mut(std::mem::take)
}

#[test]
fn physics_history_holds_external_edits_while_authored_motion_advances() {
    let mut runtime = runtime();
    assert_eq!(frame(&mut runtime, 0.0, 1.0, 0.0).len(), 1);
    let end = 1.0 / 30.0;
    let observations = frame(&mut runtime, end, 9.0, 3.0);
    assert_eq!(
        observations.len(),
        10,
        "drain the previous observation, eight historical samples and the live frame"
    );
    let (current, historical) = observations.split_last().unwrap();
    for sample in historical {
        assert_eq!(sample.values[0], 1.0);
        assert_eq!(
            sample.values[3], 0.0,
            "new trigger must not leak into old samples"
        );
        assert_eq!(sample.values[1], sample.time.seconds.0 as f32);
        assert_eq!(sample.values[2], sample.time.beats.0 as f32);
        assert!((sample.values[4] - (sample.time.seconds.0 as f32 * 12.0).sin()).abs() < 1.0e-6);
    }
    assert_eq!(
        historical.last().unwrap().time.seconds,
        current.time.seconds,
        "close the old interval at the edit timestamp"
    );
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
    let next = frame(&mut runtime, 2.0 * end, 12.0, 4.0);
    assert!(
        next[..next.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 9.0 && sample.values[3] == 3.0)
    );
}

#[test]
fn physics_history_survives_compatible_generator_rebuild() {
    let mut prior = runtime();
    frame(&mut prior, 0.0, 1.0, 0.0);
    let mut rebuilt = runtime();
    rebuilt.carry_generator_state_from(&mut prior);
    let observations = frame(&mut rebuilt, 1.0 / 30.0, 9.0, 3.0);
    assert_eq!(observations.len(), 10, "rebuild lost the open input interval");
    let (current, historical) = observations.split_last().unwrap();
    assert!(historical.iter().all(|sample| sample.values[0] == 1.0 && sample.values[3] == 0.0));
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
}

#[test]
fn physics_history_reanchors_paused_edits_and_backward_seeks() {
    let mut runtime = runtime();
    frame(&mut runtime, 1.0, 1.0, 0.0);
    let paused = frame(&mut runtime, 1.0, 2.0, 1.0);
    assert_eq!(paused.len(), 1);
    let advanced = frame(&mut runtime, 1.0 + 1.0 / 60.0, 3.0, 2.0);
    assert!(
        advanced[..advanced.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 2.0 && sample.values[3] == 1.0)
    );
    let seek = frame(&mut runtime, 0.0, 4.0, 3.0);
    assert_eq!(
        seek.len(),
        1,
        "a backward seek must not replay the old interval"
    );
    let advanced = frame(&mut runtime, 1.0 / 60.0, 5.0, 4.0);
    assert!(
        advanced[..advanced.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 4.0 && sample.values[3] == 3.0)
    );
}

#[test]
fn offline_history_drain_keeps_old_controls_and_bounds_input_batches() {
    let mut runtime = runtime();
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let observations = frame(&mut runtime, 3.0, 9.0, 3.0);
    let (current, historical) = observations.split_last().unwrap();
    assert!(!current.draining, "drain scope must not escape into the full frame");
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
    assert_eq!(historical[0].time.seconds, Seconds::ZERO);
    assert!(historical[0].draining, "drain the retained preview prefix first");
    let mut batch = 0;
    for sample in &historical[1..] {
        assert_eq!(sample.values[0], 1.0);
        assert_eq!(sample.values[3], 0.0);
        batch += 1;
        assert!(batch <= crate::node_graph::physics::AUTHORED_HISTORY_CAPACITY / 4);
        if sample.draining {
            batch = 0;
        }
    }
    assert_eq!(batch, 0, "close and drain the old interval before applying edits");
    assert!(historical.len() > crate::node_graph::physics::AUTHORED_HISTORY_CAPACITY);
}

#[test]
fn offline_history_drain_is_never_requested_by_preview_sampling() {
    let _preview = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut runtime = runtime();
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let observations = frame(&mut runtime, 3.0, 9.0, 3.0);
    assert!(observations.iter().all(|sample| !sample.draining));
    assert!(observations[0].time.seconds > Seconds::ZERO);
}
