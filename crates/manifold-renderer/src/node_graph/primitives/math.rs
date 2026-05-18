//! `node.math` — binary scalar arithmetic. Two `Scalar(F32)` inputs,
//! one output, an `op` enum selecting which operation to apply.
//!
//! Composition glue: wire two control producers (Value, LFO, etc.) in,
//! pick an operation, send the result into any scalar-input port.
//! Divide-by-zero clamps to 0.0 — control signals shouldn't ever
//! produce NaN/Inf where a renderer downstream might propagate them
//! into a shader.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const MATH_OPS: &[&str] = &["Add", "Subtract", "Multiply", "Divide", "Min", "Max"];

crate::primitive! {
    name: Math,
    type_id: "node.math",
    purpose: "Binary scalar arithmetic. Combines two control signals into one with the selected op (add / subtract / multiply / min / max / divide). Composition glue for control wires.",
    inputs: {
        a: ScalarF32 required,
        b: ScalarF32 required,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: "op",
            label: "Operation",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // Multiply — the most useful default
            range: Some((0.0, (MATH_OPS.len() - 1) as f32)),
            enum_values: MATH_OPS,
        },
    ],
    composition_notes: "Divide by ~0 clamps to 0 — control signals must never produce NaN/Inf that downstream shaders could propagate.",
    examples: [],
}

impl Primitive for Math {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let a = match ctx.inputs.scalar("a") {
            Some(ParamValue::Float(f)) => f,
            _ => return, // required input unwired — runtime would have caught this in validate
        };
        let b = match ctx.inputs.scalar("b") {
            Some(ParamValue::Float(f)) => f,
            _ => return,
        };
        let op = match ctx.params.get("op") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(MATH_OPS.len() - 1),
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MATH_OPS.len() - 1),
            _ => 2,
        };
        let out = match op {
            0 => a + b,
            1 => a - b,
            2 => a * b,
            3 => {
                if b.abs() < 1e-9 {
                    0.0
                } else {
                    a / b
                }
            }
            4 => a.min(b),
            _ => a.max(b),
        };
        ctx.outputs.set_scalar("out", ParamValue::Float(out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::effect_node::{EffectNode, EffectNodeType, FrameTime};
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
    use crate::node_graph::primitives::Value;
    use crate::node_graph::Executor;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    struct Capture {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::Mutex<Option<ParamValue>>>,
    }
    impl EffectNode for Capture {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: "in",
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

    fn run_math(a: f32, b: f32, op_idx: u32) -> f32 {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let va = g.add_node(Box::new(Value::new()));
        let vb = g.add_node(Box::new(Value::new()));
        let math = g.add_node(Box::new(Math::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(va, "value", ParamValue::Float(a)).unwrap();
        g.set_param(vb, "value", ParamValue::Float(b)).unwrap();
        g.set_param(math, "op", ParamValue::Enum(op_idx)).unwrap();
        g.connect((va, "out"), (math, "a")).unwrap();
        g.connect((vb, "out"), (math, "b")).unwrap();
        g.connect((math, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        match *seen.lock().unwrap() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("Math did not emit a Float: {v:?}"),
        }
    }

    #[test]
    fn add() {
        assert!((run_math(0.3, 0.4, 0) - 0.7).abs() < 1e-5);
    }
    #[test]
    fn subtract() {
        assert!((run_math(0.7, 0.3, 1) - 0.4).abs() < 1e-5);
    }
    #[test]
    fn multiply() {
        assert!((run_math(0.5, 0.5, 2) - 0.25).abs() < 1e-5);
    }
    #[test]
    fn divide() {
        assert!((run_math(0.6, 0.2, 3) - 3.0).abs() < 1e-4);
    }
    #[test]
    fn divide_by_zero_clamps_to_zero() {
        assert_eq!(run_math(0.6, 0.0, 3), 0.0);
    }
    #[test]
    fn min() {
        assert!((run_math(0.3, 0.7, 4) - 0.3).abs() < 1e-5);
    }
    #[test]
    fn max() {
        assert!((run_math(0.3, 0.7, 5) - 0.7).abs() < 1e-5);
    }
}
