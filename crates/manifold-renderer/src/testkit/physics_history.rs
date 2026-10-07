use crate::preset_runtime::*;
use crate::node_graph::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use std::{borrow::Cow, cell::Cell};
thread_local! {
    pub(crate) static FLUID_TIME: Cell<Option<f32>> = const { Cell::new(None) };
}

pub(crate) struct FluidTimeObserver(pub(crate) EffectNodeType);

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

pub(crate) fn runtime_definition() -> EffectGraphDef {
    let def = serde_json::json!({
        "version": 2, "name": "Fluid offline history",
        "nodes": [
            {"id":0,"nodeId":"fluid","typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,"params":{
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
    serde_json::from_value(def).unwrap()
}

pub(crate) fn runtime_from_definition(def: EffectGraphDef) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.fluid_time", || {
        Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
    });
    PresetRuntime::from_def(def, &registry, None).unwrap()
}

pub(crate) fn runtime() -> PresetRuntime {
    runtime_from_definition(runtime_definition())
}

