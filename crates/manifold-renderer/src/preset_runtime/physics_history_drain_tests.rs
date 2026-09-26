//! Native FLIP history catch-up through the ordinary CPU graph path.
use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef};
use std::{borrow::Cow, cell::Cell};

thread_local! {
    static FLUID_TIME: Cell<Option<f32>> = const { Cell::new(None) };
}

struct FluidTimeObserver(EffectNodeType);

impl EffectNode for FluidTimeObserver {
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
            name: Cow::Borrowed("time"),
            ty: PortType::Scalar(ScalarType::F32),
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
        assert!(
            !crate::node_graph::physics::authored_sample_only(),
            "downstream consumers must only run in the full frame"
        );
        FLUID_TIME.set(
            ctx.inputs
                .scalar("time")
                .and_then(|value| value.as_scalar()),
        );
    }
}

#[test]
fn offline_history_drain_crosses_fluid_history_capacity_without_reset() {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.fluid_time", || {
        Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Fluid offline history",
        "nodes": [
            {"id":0,"nodeId":"fluid","typeId":"node.fluid_surface","params":{
                "resolution":{"type":"Int","value":8},
                "fill_height":{"type":"Float","value":0.0},
                "emission":{"type":"Float","value":0.0}
            }},
            {"id":1,"nodeId":"observe","typeId":"test.fluid_time"},
            {"id":2,"nodeId":"source","typeId":"system.source"},
            {"id":3,"nodeId":"output","typeId":"system.final_output"},
            {"id":4,"nodeId":"input","typeId":"system.generator_input"}
        ],
        "wires": [
            {"fromNode":0,"fromPort":"simulation_time","toNode":1,"toPort":"time"},
            {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"}
        ]
    });
    let mut runtime = PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap();
    // 36 seconds generates over 8192 authored endpoints. An empty domain keeps
    // this a bounded scheduling proof; liquid motion is covered separately.
    let time = |seconds: f64| FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(seconds),
        frame_count: (seconds * 60.0) as i64,
    };
    runtime.execute_frame(time(0.0));
    assert_eq!(FLUID_TIME.get(), Some(0.0));
    FLUID_TIME.set(None);
    runtime.sample_physics_history(time(36.0));
    assert_eq!(
        FLUID_TIME.get(),
        None,
        "history must not run the output consumer"
    );
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    let resource = runtime
        .plan
        .steps()
        .iter()
        .find(|step| step.node == fluid)
        .unwrap()
        .outputs
        .iter()
        .find(|(name, _)| *name == "simulation_time")
        .unwrap()
        .1;
    let backend = runtime.executor.backend();
    let published = backend
        .slot_for(resource)
        .and_then(|slot| backend.scalar(slot))
        .and_then(|value| value.as_scalar());
    assert!(
        published.is_none_or(|value| value == 0.0),
        "intermediate native progress must not publish graph outputs: {published:?}"
    );
    runtime
        .executor
        .execute_frame(&mut runtime.graph, &runtime.plan, time(36.0));
    assert_eq!(FLUID_TIME.get(), Some(36.0));
}
