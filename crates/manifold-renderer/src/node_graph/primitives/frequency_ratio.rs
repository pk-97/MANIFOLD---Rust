//! `node.frequency_ratio` — emit two scalars from a curated table
//! of small-integer harmonic ratios.
//!
//! The clip-trigger primitive for shape generators that want clean,
//! musically-meaningful closed curves. Indexing a single `index`
//! into a 10-row table outputs an `(a, b)` pair where the ratio
//! `a : b` maps to a recognisable musical interval (1:2 octave,
//! 2:3 fifth, 3:4 fourth, …). For Lissajous curves these ratios
//! produce visually clean closed shapes — non-integer ratios just
//! fill the box with a non-closing scribble.
//!
//! Index is `port-shadows-param`: drive it from a counter / trigger
//! source for clip-trigger-stepped harmonic variety per retrigger, or
//! pin it as a constant param when authoring a fixed shape.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Curated 10-row harmonic ratio table. Each row is `(a, b)` —
/// the integer-frequency pair for the Lissajous curve `(sin(a*t),
/// sin(b*t))`. Lifted verbatim from the legacy LissajousGenerator's
/// trigger-cycling tables so the clip-trigger mode of the decomposed
/// Lissajous graph
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
    purpose: "Emit two scalars from a curated table of small-integer harmonic ratios. `a:b` maps to a musical interval (1:2 octave, 2:3 fifth, 3:4 fourth, …). Indexing the 10-row table is the clip-trigger primitive for shape generators (Lissajous-style curves) that want clean musically-meaningful closed shapes instead of non-closing scribbles. `index` is port-shadows-param so a counter / trigger source can drive the variety per retrigger.",
    inputs: {
        index: ScalarF32 optional,
    },
    outputs: {
        a: ScalarF32,
        b: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("index"),
            label: "Index",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, (FREQUENCY_RATIO_TABLE.len() - 1) as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Index rounds to nearest integer and wraps modulo 10, so wiring an unbounded counter into the input port cycles through the table. The table is small-integer ratios chosen for visually-clean closed Lissajous curves — for non-Lissajous uses the same harmonic vocabulary still produces musically-coherent outputs.",
    examples: [],
    picker: { label: "Frequency Ratio", category: Driver },
    summary: "Emits a pair of small whole-number ratios from a musical-interval table. Use it for Lissajous curves and similar shapes where the X and Y rates set the form.",
    category: Control,
    role: Control,
    aliases: ["frequency ratio", "harmonic", "interval"],
    boundary_reason: NonGpu,
    extra_fields: {
        clip_trigger_cycle: crate::generators::clip_trigger::ClipTriggerCycle = crate::generators::clip_trigger::ClipTriggerCycle::new(),
    },
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
                Some(ParamValue::Float(f)) => *f,
                Some(ParamValue::Enum(v)) => *v as f32,
                _ => 0.0,
            },
        };
        let rounded = raw_index.round() as i64;
        let len = FREQUENCY_RATIO_TABLE.len() as u32;
        // Pass the raw, un-wrapped count to the cycle. The cycle's
        // idempotence detection compares `last_trigger_count` to
        // the input — if we pre-wrap (`% len`) here, two distinct
        // trigger events that happen to share a wrapped index
        // (e.g. counts 1 and 11) look identical to the cycle and
        // it stalls on the cached emission instead of advancing.
        // Negative inputs clamp to 0; counters are conventionally
        // non-negative, and the cycle's u32 input precludes
        // signed values.
        let count = rounded.max(0) as u32;
        let idx = self.clip_trigger_cycle.step(count, len);
        let (a, b) = FREQUENCY_RATIO_TABLE[idx as usize];

        ctx.outputs.set_scalar("a", ParamValue::Float(a));
        ctx.outputs.set_scalar("b", ParamValue::Float(b));
    }

    /// BUG-104: `clip_trigger_cycle` holds the last-emitted row index
    /// forever (by design — never repeating consecutive rows) with no
    /// self-driven way back to a fresh state. Reset it to `new()` so the
    /// next trigger after release starts idempotence tracking over, the
    /// same as a freshly-built graph.
    fn clear_state(&mut self) {
        self.clip_trigger_cycle = crate::generators::clip_trigger::ClipTriggerCycle::new();
    }

    fn is_trigger_latch(&self) -> bool {
        true
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
        g.set_param(ratio, "index", ParamValue::Float(index as f32)).unwrap();
        g.connect((ratio, "a"), (sink_a, "in")).unwrap();
        g.connect((ratio, "b"), (sink_b, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        let a = match seen_a.lock().unwrap().clone() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("a port did not emit a Float: {v:?}"),
        };
        let b = match seen_b.lock().unwrap().clone() {
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

    /// Positive index above table length wraps modulo 10 via the
    /// uniqueness cycle. `12 % 10 = 2` → row 2 (2:3 fifth).
    #[test]
    fn positive_index_wraps_modulo_table_length() {
        let (a, b) = run_ratio(12);
        assert_eq!((a, b), (2.0, 3.0));
    }

    /// Negative indices clamp to 0 rather than wrapping. The cycle's
    /// `step()` takes `u32`, so signed inputs collapse to zero
    /// here; counters that drive this primitive are conventionally
    /// non-negative (trigger_count from `system.generator_input` is
    /// always >= 0). Locked in so a future refactor doesn't reach
    /// for the old `((x % len) + len) % len` formula — that breaks
    /// the cycle's idempotence detection by collapsing distinct
    /// trigger events that share a wrapped index.
    #[test]
    fn negative_index_clamps_to_zero() {
        let (a, b) = run_ratio(-8);
        assert_eq!((a, b), (1.0, 2.0));
    }

    /// Bit-perfect match with the legacy LissajousGenerator's
    /// trigger-cycling tables (the legacy LissajousGenerator's
    /// per-retrigger ratio arrays). The whole point of this primitive
    /// is that clip-trigger rows match the legacy table.
    #[test]
    fn matches_legacy_clip_trigger_table_row_for_row() {
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
        g.set_param(ratio, "index", ParamValue::Float(7.0)).unwrap(); // would be 5:8 if param wins
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
        let a = match seen_a.lock().unwrap().clone() {
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

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let prim = FrequencyRatio::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }

    /// BUG-104 — proves `clear_state()` (what `PresetRuntime::
    /// clear_trigger_state` calls on every `is_trigger_latch` node) actually
    /// releases the cycle's idempotence cache, through the same
    /// `EffectNode` trait object the runtime uses (not a direct field poke).
    #[test]
    fn clear_state_releases_the_cycle_idempotence_cache() {
        let mut prim = FrequencyRatio::new();
        assert_eq!(prim.clip_trigger_cycle.step(0, 10), 0);
        // 10 % 10 == 0 would repeat the previous emission (0) — the
        // anti-repeat guard advances to 1.
        assert_eq!(prim.clip_trigger_cycle.step(10, 10), 1);
        // Idempotent: the SAME trigger_count re-queried without an
        // intervening advance returns the cached emission, not a fresh
        // computation.
        assert_eq!(prim.clip_trigger_cycle.step(10, 10), 1);

        {
            let node: &mut dyn EffectNode = &mut prim;
            node.clear_state();
        }

        // Same raw trigger_count as the cached call above, but with the
        // cycle released there is no "previous emission" to compare
        // against — the anti-repeat guard doesn't fire (it only applies
        // after a first observation), so this is a fresh 10 % 10 = 0,
        // not the stale cached 1. This is exactly the "stays dead after
        // Trigger is disabled" half of BUG-104 for a mux-selector cycle.
        assert_eq!(prim.clip_trigger_cycle.step(10, 10), 0);
    }
}
