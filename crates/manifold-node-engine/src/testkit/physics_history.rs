use crate::exec::effect_node::EffectNode;
use crate::exec::effect_node::EffectNodeContext;
use crate::exec::effect_node::EffectNodeType;
use crate::parameters::ParamDef;
use crate::persistence::PrimitiveRegistry;
use crate::runtime::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
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
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
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
            !crate::water::physics::authored_sample_only(),
            "downstream consumers must only run in the full frame"
        );
        FLUID_TIME.set(
            ctx.inputs
                .scalar("time")
                .and_then(|value| value.as_scalar()),
        );
    }
}

pub fn runtime_definition() -> EffectGraphDef {
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

pub fn runtime_from_definition(def: EffectGraphDef) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    registry.register("test.fluid_time", || {
        Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
    });
    PresetRuntime::from_def(def, &registry, None).unwrap()
}

#[cfg(test)]
pub(crate) fn runtime() -> PresetRuntime {
    runtime_from_definition(runtime_definition())
}


pub fn observed_fluid_time() -> Option<f32> { FLUID_TIME.get() }
pub fn set_observed_fluid_time(value: Option<f32>) { FLUID_TIME.set(value); }
#[cfg(test)]
pub(crate) fn fluid_time_observer() -> Box<dyn EffectNode> {
    Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
}
