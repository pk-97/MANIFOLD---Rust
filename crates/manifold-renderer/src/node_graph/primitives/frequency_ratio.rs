//! `node.frequency_ratio` — emit two scalars from a curated table
//! of small-integer harmonic ratios.
//!
//! The snap-mode primitive for shape generators that want clean,
//! musically-meaningful closed curves. Indexing a single `index`
//! into a 10-row table outputs an `(a, b)` pair where the ratio
//! `a : b` maps to a recognisable musical interval (1:2 octave,
//! 2:3 fifth, 3:4 fourth, …). For Lissajous curves these ratios
//! produce visually clean closed shapes — non-integer ratios just
//! fill the box with a non-closing scribble.
//!
//! Index is `port-shadows-param`: drive it from a counter / trigger
//! source for snap-stepped harmonic variety per clip retrigger, or
//! pin it as a constant param when authoring a fixed shape.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Curated 10-row harmonic ratio table. Each row is `(a, b)` —
/// the integer-frequency pair for the Lissajous curve `(sin(a*t),
/// sin(b*t))`. Lifted from `LissajousGenerator::SNAP_A` /
/// `SNAP_B` so the snap mode of the decomposed Lissajous graph
/// matches the legacy generator row-for-row.
pub const FREQUENCY_RATIO_TABLE: [(f32, f32); 10] = [
    (1.0, 2.0), // 0 — octave             (figure-8)
    (1.0, 3.0), // 1 — octave + fifth
    (2.0, 3.0), // 2 — perfect fifth
    (3.0, 4.0), // 3 — perfect fourth
    (3.0, 5.0), // 4 — major sixth
    (4.0, 5.0), // 5 — major third
    (5.0, 6.0), // 6 — minor third
    (5.0, 8.0), // 7 — minor sixth
    (7.0, 8.0), // 8 — dissonant near-unison
    (3.0, 7.0), // 9 — dissonant 3:7
];

crate::primitive! {
    name: FrequencyRatio,
    type_id: "node.frequency_ratio",
    purpose: "Emit two scalars from a curated table of small-integer harmonic ratios. `a:b` maps to a musical interval (1:2 octave, 2:3 fifth, 3:4 fourth, …). Indexing the 10-row table is the snap-mode primitive for shape generators (Lissajous-style curves) that want clean musically-meaningful closed shapes instead of non-closing scribbles. `index` is port-shadows-param so a counter / trigger source can drive snap-stepped variety per retrigger.",
    inputs: {
        index: ScalarF32 optional,
    },
    outputs: {
        a: ScalarF32,
        b: ScalarF32,
    },
    params: [
        ParamDef {
            name: "index",
            label: "Index",
            ty: ParamType::Int,
            default: ParamValue::Int(0),
            range: Some((0.0, (FREQUENCY_RATIO_TABLE.len() - 1) as f32)),
            enum_values: &[],
        },
    ],
    composition_notes: "Index rounds to nearest integer and wraps modulo 10, so wiring an unbounded counter into the input port cycles through the table. The table is small-integer ratios chosen for visually-clean closed Lissajous curves — for non-Lissajous uses the same harmonic vocabulary still produces musically-coherent outputs.",
    examples: [],
    picker: { label: "Frequency Ratio", category: Driver },
}

impl Primitive for FrequencyRatio {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param for `index`: a wired counter wins over
        // the inline param. Float / Int / Enum all collapse to a
        // rounded integer mod 10 so any plausible upstream source
        // (counter, LFO output, trigger-count) selects sensibly.
        let raw_index = match ctx.inputs.scalar("index") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("index") {
                Some(ParamValue::Int(i)) => *i as f32,
                Some(ParamValue::Float(f)) => *f,
                Some(ParamValue::Enum(v)) => *v as f32,
                _ => 0.0,
            },
        };
        let rounded = raw_index.round() as i64;
        let len = FREQUENCY_RATIO_TABLE.len() as i64;
        // Rust's `%` keeps the sign of the dividend; force a
        // non-negative index so negative counters wrap cleanly.
        let idx = ((rounded % len) + len) % len;
        let (a, b) = FREQUENCY_RATIO_TABLE[idx as usize];

        ctx.outputs.set_scalar("a", ParamValue::Float(a));
        ctx.outputs.set_scalar("b", ParamValue::Float(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::Value;
    use crate::node_graph::{Executor, FrameTime, Graph, compile};
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};

    /// Scalar sink that captures the last value seen on its `in` port.
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

    /// Drive the FrequencyRatio with an inline `index` param and
    /// capture both outputs through downstream Capture sinks.
    fn run_ratio(index: i32) -> (f32, f32) {
        let seen_a = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen_b = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let ratio = g.add_node(Box::new(FrequencyRatio::new()));
        let sink_a = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.cap_a"),
            seen: seen_a.clone(),
        }));
        let sink_b = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.cap_b"),
            seen: seen_b.clone(),
        }));
        g.set_param(ratio, "index", ParamValue::Int(index)).unwrap();
        g.connect((ratio, "a"), (sink_a, "in")).unwrap();
        g.connect((ratio, "b"), (sink_b, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        let a = match *seen_a.lock().unwrap() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("a port did not emit a Float: {v:?}"),
        };
        let b = match *seen_b.lock().unwrap() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("b port did not emit a Float: {v:?}"),
        };
        (a, b)
    }

    #[test]
    fn declares_one_optional_scalar_input_and_two_scalar_outputs() {
        let inputs = FrequencyRatio::INPUTS;
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "index");
        assert!(!inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));

        let outputs = FrequencyRatio::OUTPUTS;
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].name, "a");
        assert_eq!(outputs[1].name, "b");
        for port in outputs {
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn index_zero_emits_octave() {
        let (a, b) = run_ratio(0);
        assert_eq!((a, b), (1.0, 2.0));
    }

    #[test]
    fn index_two_emits_perfect_fifth() {
        let (a, b) = run_ratio(2);
        assert_eq!((a, b), (2.0, 3.0));
    }

    /// Out-of-range index wraps modulo 10. Both `12` and `-8` should
    /// land on row 2 (2:3 fifth).
    #[test]
    fn index_wraps_modulo_table_length() {
        let (a, b) = run_ratio(12);
        assert_eq!((a, b), (2.0, 3.0));
        let (a, b) = run_ratio(-8);
        assert_eq!((a, b), (2.0, 3.0));
    }

    /// Bit-perfect match with the legacy LissajousGenerator's
    /// `SNAP_A` and `SNAP_B` arrays. The whole point of this
    /// primitive is that snap-mode rows match the legacy table.
    #[test]
    fn matches_legacy_snap_table_row_for_row() {
        const LEGACY_A: [f32; 10] = [1.0, 1.0, 2.0, 3.0, 3.0, 4.0, 5.0, 5.0, 7.0, 3.0];
        const LEGACY_B: [f32; 10] = [2.0, 3.0, 3.0, 4.0, 5.0, 5.0, 6.0, 8.0, 8.0, 7.0];
        for i in 0..10 {
            let (a, b) = run_ratio(i as i32);
            assert_eq!(
                (a, b),
                (LEGACY_A[i], LEGACY_B[i]),
                "row {i}: graph emits ({a}, {b}), legacy expects ({}, {})",
                LEGACY_A[i],
                LEGACY_B[i],
            );
        }
    }

    /// Port-shadows-param: a wired scalar value drives the index,
    /// overriding the inline param. Setting param=0 and wiring
    /// Value=2 should select row 2 (the fifth).
    #[test]
    fn wired_index_input_overrides_inline_param() {
        let seen_a = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let v = g.add_node(Box::new(Value::new()));
        g.set_param(v, "value", ParamValue::Float(2.0)).unwrap();
        let ratio = g.add_node(Box::new(FrequencyRatio::new()));
        g.set_param(ratio, "index", ParamValue::Int(7)).unwrap(); // would be 5:8 if param wins
        let sink_a = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.cap_a"),
            seen: seen_a.clone(),
        }));
        g.connect((v, "out"), (ratio, "index")).unwrap();
        g.connect((ratio, "a"), (sink_a, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        // Row 2 (a=2.0) wins because the wire takes priority.
        let a = match *seen_a.lock().unwrap() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("a did not emit a Float: {v:?}"),
        };
        assert_eq!(a, 2.0, "wired index=2 should select row 2 (a=2.0), not param=7 (a=5.0)");
    }

    #[test]
    fn primitive_registers_as_palette_driver() {
        let prim = FrequencyRatio::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.frequency_ratio");
    }
}
