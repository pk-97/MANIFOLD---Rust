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

pub const MATH_OPS: &[&str] = &[
    "Add",        // 0
    "Subtract",   // 1
    "Multiply",   // 2
    "Divide",     // 3
    "Min",        // 4
    "Max",        // 5
    "Atan2",      // 6
    "Sin",        // 7 — unary, b ignored
    "Cos",        // 8 — unary, b ignored
    "Reciprocal", // 9 — unary, b ignored; 1/a with 0-clamp
];

crate::primitive! {
    name: Math,
    type_id: "node.math",
    purpose: "Scalar arithmetic. Combines two control signals into one with the selected op (add / subtract / multiply / divide / min / max / atan2 / sin / cos). Composition glue for control wires. `b` is unused for unary ops (sin, cos). Both `a` and `b` are port-shadows-param: when an input wire isn't connected the inline param value is used, so constants can be set on the node without dragging a Value node in.",
    inputs: {
        a: ScalarF32 required,
        b: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: "a",
            label: "A",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "b",
            label: "B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "op",
            label: "Operation",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // Multiply — the most useful default
            range: Some((0.0, (MATH_OPS.len() - 1) as f32)),
            enum_values: MATH_OPS,
        },
    ],
    composition_notes: "Divide by ~0 clamps to 0 — control signals must never produce NaN/Inf that downstream shaders could propagate. Sin and Cos are unary ops that read `a` only (in radians) and ignore `b`; convenient for deriving rotation coefficients from time wires when composing rotating procedural fields.",
    examples: [],
    picker: { label: "Math", category: Driver },
}

fn read_scalar(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.inputs.scalar(name) {
        Some(ParamValue::Float(f)) => f,
        _ => match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        },
    }
}

impl Primitive for Math {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param for both inputs: wired scalar overrides
        // the inline param, otherwise the param's static value drives
        // the op. Constants embedded in the graph live as param values
        // on this node instead of as separate Value-node wires.
        let a = read_scalar(ctx, "a", 0.0);
        let op = match ctx.params.get("op") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(MATH_OPS.len() - 1),
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as usize).min(MATH_OPS.len() - 1),
            _ => 2,
        };

        // Unary ops compute from `a` only.
        if op == 7 {
            ctx.outputs.set_scalar("out", ParamValue::Float(a.sin()));
            return;
        }
        if op == 8 {
            ctx.outputs.set_scalar("out", ParamValue::Float(a.cos()));
            return;
        }
        if op == 9 {
            let out = if a.abs() < 1e-9 { 0.0 } else { 1.0 / a };
            ctx.outputs.set_scalar("out", ParamValue::Float(out));
            return;
        }

        let b = read_scalar(ctx, "b", 0.0);
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
            5 => a.max(b),
            // Atan2(a, b) → angle of the (b, a) → (x, y) vector in
            // radians. Convention: input `a` is y, input `b` is x —
            // matches Rust / WGSL / std-library `atan2(y, x)`.
            // Returns 0 when both inputs are zero rather than letting
            // the platform pick a value the renderer might propagate.
            _ => {
                if a == 0.0 && b == 0.0 {
                    0.0
                } else {
                    a.atan2(b)
                }
            }
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

    /// `Atan2(a, b)` follows the std / WGSL convention of `atan2(y, x)`.
    /// `atan2(1, 0) = π/2`, `atan2(0, 1) = 0`, `atan2(0, -1) = π`,
    /// `atan2(-1, 0) = -π/2`. These four cardinal cases cover sign
    /// conventions in all four quadrants — wires the Color Compass
    /// builds on top of this op.
    #[test]
    fn atan2_north_is_half_pi() {
        let got = run_math(1.0, 0.0, 6);
        assert!(
            (got - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "atan2(1, 0) = π/2, got {got}",
        );
    }
    #[test]
    fn atan2_east_is_zero() {
        assert!((run_math(0.0, 1.0, 6)).abs() < 1e-5);
    }
    #[test]
    fn atan2_west_is_pi() {
        let got = run_math(0.0, -1.0, 6);
        assert!(
            (got - std::f32::consts::PI).abs() < 1e-5,
            "atan2(0, -1) = π, got {got}",
        );
    }
    #[test]
    fn atan2_south_is_negative_half_pi() {
        let got = run_math(-1.0, 0.0, 6);
        assert!(
            (got + std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "atan2(-1, 0) = -π/2, got {got}",
        );
    }
    /// Both inputs zero → 0.0 explicit (not whatever the platform's
    /// undefined `atan2(0, 0)` returns). Prevents NaN-propagation
    /// into downstream parameters when the compass has no asymmetry
    /// at all (e.g. uniform brightness across all four cardinals).
    #[test]
    fn atan2_zero_zero_clamps_to_zero() {
        assert_eq!(run_math(0.0, 0.0, 6), 0.0);
    }

    /// Sin / Cos are unary ops — read `a` (in radians), ignore `b`.
    /// The Math primitive's `b` port is optional now; these tests
    /// wire `b = 0` for ergonomic parity with the binary-op tests.
    #[test]
    fn sin_zero_is_zero() {
        assert!(run_math(0.0, 0.0, 7).abs() < 1e-6);
    }
    #[test]
    fn sin_half_pi_is_one() {
        let got = run_math(std::f32::consts::FRAC_PI_2, 0.0, 7);
        assert!((got - 1.0).abs() < 1e-6, "sin(π/2) = 1, got {got}");
    }
    #[test]
    fn cos_zero_is_one() {
        assert!((run_math(0.0, 0.0, 8) - 1.0).abs() < 1e-6);
    }
    #[test]
    fn cos_pi_is_negative_one() {
        let got = run_math(std::f32::consts::PI, 0.0, 8);
        assert!((got - -1.0).abs() < 1e-6, "cos(π) = -1, got {got}");
    }
}
