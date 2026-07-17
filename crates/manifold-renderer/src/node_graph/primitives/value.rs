//! `node.value` — emit a constant scalar value as a control wire.
//!
//! The simplest possible control primitive. Drives a `Scalar(F32)`
//! output from a single `value` parameter. Wire its output into any
//! same-typed scalar input port (e.g. `wet_dry_mix.wet_dry`) and the
//! consumer reads the live value through
//! [`NodeInputs::scalar`](crate::node_graph::NodeInputs::scalar)
//! instead of falling back to its own static param.
//!
//! Value is the validation primitive for the control-wire plumbing —
//! constant-only for now. LFOs, math operators, beat-locked sources
//! and audio bridges land in subsequent slices.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Value,
    type_id: "node.value",
    purpose: "Emit a constant scalar value on the `out` port. Drive any scalar input by wiring this in; the consumer's same-named param becomes the fallback for when the wire is absent.",
    inputs: {},
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("value"),
            label: "Value",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First-class building block of the control-wire surface. Future LFO / Math / Beat primitives slot in next to this with the same Scalar(F32) output shape.",
    examples: [],
    picker: { label: "Value", category: Driver },
    summary: "Outputs a single fixed number you set by hand. Wire it into any knob as a constant, or expose it to drive from outside.",
    category: Control,
    role: Source,
    aliases: ["value", "constant", "Constant CHOP"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for Value {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let value = match ctx.params.get("value") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        ctx.outputs.set_scalar("out", ParamValue::Float(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{Executor, FrameTime, Graph, ParamValue, compile};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// The `value` param feeds straight through to the `out` scalar. A
    /// downstream consumer reads it through `NodeInputs::scalar` once
    /// the executor has drained the per-step write scratch.
    #[test]
    fn value_writes_param_to_scalar_output() {
        use crate::node_graph::effect_node::{EffectNode, EffectNodeType};
        use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};

        // Hand-built consumer that just records what scalar it sees on
        // its input port. Avoids dragging the wet_dry shader in for a
        // CPU-side wiring test.
        struct ScalarSink {
            type_id: EffectNodeType,
            seen: std::sync::Arc<std::sync::Mutex<Option<ParamValue>>>,
        }
        impl EffectNode for ScalarSink {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: Cow::Borrowed("in"),
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
                *self.seen.lock().unwrap() = ctx.inputs.scalar("in");
            }
        }

        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let value = g.add_node(Box::new(Value::new()));
        let sink = g.add_node(Box::new(ScalarSink {
            type_id: EffectNodeType::new("test.scalar_sink"),
            seen: seen.clone(),
        }));
        g.set_param(value, "value", ParamValue::Float(0.42)).unwrap();
        g.connect((value, "out"), (sink, "in")).unwrap();

        // No FinalOutput in this graph — the executor walks every node
        // anyway when there's no boundary to filter against, which is
        // what we want for a wiring smoke test.
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let observed = seen.lock().unwrap().clone();
        assert!(
            matches!(observed, Some(ParamValue::Float(f)) if (f - 0.42).abs() < 1e-6),
            "expected ScalarSink to see 0.42 through Value→sink wire, got {observed:?}"
        );
        let _ = sink;
    }
}
