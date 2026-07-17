//! `node.beat_gate` — beat-synced square gate emitting `0` or `amount`
//! on its `out` scalar port.
//!
//! The musician-facing name for what is mathematically a square LFO at
//! the same note rate. Ships as its own primitive (rather than living
//! inside `node.lfo`) because "BeatGate" is the discoverable name in
//! the picker for users who want a strobe-style gate signal —
//! "Square-shape LFO" is the same math but worse vocabulary.
//!
//! Stateless: phase is `fract(beats * rate + phase_offset)` computed
//! fresh each frame from [`FrameTime::beats`]. The gate is on for the
//! second half of each cycle (`phase >= duty`); `duty` is the
//! fractional position within the cycle where the gate flips on,
//! defaulting to `0.5` for the conventional 50/50 strobe. `amount`
//! scales the on-state value, so a wired `amount` lets an external
//! envelope or audio level modulate strobe depth.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::primitives::note_rates::NOTE_RATE_VALUES;

/// Display labels for the `rate` enum. Indices match
/// [`NOTE_RATE_VALUES`] (the shared note-rate table). Kept in sync with
/// `node.lfo`'s rate vocabulary.
pub const BEAT_GATE_RATE_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64",
];

crate::primitive! {
    name: BeatGate,
    type_id: "node.beat_gate",
    purpose: "Beat-synced square gate. Outputs `0` when off and `amount` when on, flipping at the `duty` point within each cycle of the selected note rate. Stateless and seek-safe — the same musical pattern as `node.strobe`'s internal gate, surfaced as a wireable scalar source.",
    inputs: {},
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Rate",
            ty: ParamType::Enum,
            default: ParamValue::Enum(6), // "1/16" — matches Strobe's default
            range: Some((0.0, (BEAT_GATE_RATE_LABELS.len() - 1) as f32)),
            enum_values: BEAT_GATE_RATE_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("duty"),
            label: "Duty",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("phase"),
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output is `0` or `amount` (not blended). For smooth-edged strobes, wire through `node.lfo` (square shape) and a slew/smoothing primitive instead. `duty` defaults to 0.5 — matches the gate in `node.strobe`. Wiring an LFO or envelope into `amount` produces a beat-quantised modulated gate.",
    examples: [],
    picker: { label: "Beat Gate", category: Driver },
    summary: "A square pulse locked to the tempo, on for part of each beat and off for the rest. The strobe and chop building block.",
    category: Control,
    role: Control,
    aliases: ["beat gate", "strobe", "tempo gate", "chop"],
    boundary_reason: NonGpu,
}

impl Primitive for BeatGate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let rate_idx = match ctx.params.get("rate") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(NOTE_RATE_VALUES.len() - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as usize).min(NOTE_RATE_VALUES.len() - 1)
            }
            _ => 6,
        };
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let duty = match ctx.params.get("duty") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 0.5,
        };
        let phase_offset = match ctx.params.get("phase") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let cycles_per_beat = NOTE_RATE_VALUES[rate_idx];
        let beats = ctx.time.beats.0 as f32;
        let mut p = (beats * cycles_per_beat + phase_offset).fract();
        if p < 0.0 {
            p += 1.0;
        }
        // Match the legacy Strobe shader's `step(0.5, phase)` exactly when
        // `duty == 0.5` — gate is on for `phase >= duty`, off otherwise.
        // Bit-for-bit identical to the inline gate inside `node.strobe`
        // for the upcoming Strobe-as-graph migration's parity test.
        let on = if p >= duty { 1.0 } else { 0.0 };
        ctx.outputs.set_scalar("out", ParamValue::Float(amount * on));
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
    use crate::node_graph::Executor;

    fn frame_at_beats(b: f32) -> FrameTime {
        FrameTime {
            beats: Beats(b as f64),
            seconds: Seconds((b as f64) * 0.5),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

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

    fn drive_gate(rate_idx: u32, amount: f32, duty: f32, phase: f32, beats: f32) -> f32 {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let gate = g.add_node(Box::new(BeatGate::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(gate, "rate", ParamValue::Enum(rate_idx)).unwrap();
        g.set_param(gate, "amount", ParamValue::Float(amount)).unwrap();
        g.set_param(gate, "duty", ParamValue::Float(duty)).unwrap();
        g.set_param(gate, "phase", ParamValue::Float(phase)).unwrap();
        g.connect((gate, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_at_beats(beats));
        match seen.lock().unwrap().clone() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("BeatGate did not emit a Float: {v:?}"),
        }
    }

    #[test]
    fn gate_off_in_first_half_of_cycle_default_duty() {
        // rate=2 (1/4 = 1 cycle/beat), duty=0.5. At beats=0.1 → phase=0.1 < 0.5 → off.
        assert_eq!(drive_gate(2, 1.0, 0.5, 0.0, 0.1), 0.0);
    }

    #[test]
    fn gate_on_in_second_half_of_cycle_default_duty() {
        // rate=2, duty=0.5. At beats=0.7 → phase=0.7 >= 0.5 → on at amount.
        assert!((drive_gate(2, 1.0, 0.5, 0.0, 0.7) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn amount_scales_on_state() {
        // amount=0.3, on phase → output = 0.3.
        assert!((drive_gate(2, 0.3, 0.5, 0.0, 0.7) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn duty_shifts_gate_threshold() {
        // duty=0.25, at beats=0.3 → phase=0.3 >= 0.25 → on. At beats=0.1 → off.
        assert!((drive_gate(2, 1.0, 0.25, 0.0, 0.3) - 1.0).abs() < 1e-6);
        assert_eq!(drive_gate(2, 1.0, 0.25, 0.0, 0.1), 0.0);
    }

    #[test]
    fn phase_offset_shifts_cycle() {
        // phase=0.5, at beats=0 → effective phase=0.5 → on (>= 0.5 duty).
        assert!((drive_gate(2, 1.0, 0.5, 0.5, 0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rate_governs_cycle_frequency() {
        // rate=0 (1/1 = 0.25 cycles/beat → 4 beats per cycle).
        // At beats=1.0 → phase = fract(1.0 * 0.25) = 0.25 → off (< 0.5).
        // At beats=3.0 → phase = fract(3.0 * 0.25) = 0.75 → on.
        assert_eq!(drive_gate(0, 1.0, 0.5, 0.0, 1.0), 0.0);
        assert!((drive_gate(0, 1.0, 0.5, 0.0, 3.0) - 1.0).abs() < 1e-6);
    }

    /// Bit-for-bit match with the inline gate in `node.strobe`'s shader:
    /// `phase = fract(beat * rate); on = step(0.5, phase); strobe = amount * on`.
    /// This is the parity test that backs the §12.6 Strobe-as-graph claim.
    #[test]
    fn matches_legacy_strobe_inline_gate() {
        // Strobe defaults: rate=6 (1/16 → 4 cycles/beat), amount=1.0, duty=0.5.
        for beat_step in 0..20 {
            let beats = beat_step as f32 * 0.1; // 0.0, 0.1, …, 1.9
            let phase = (beats * NOTE_RATE_VALUES[6]).fract();
            let expected_on = if phase >= 0.5 { 1.0 } else { 0.0 };
            let gate = drive_gate(6, 1.0, 0.5, 0.0, beats);
            assert!(
                (gate - expected_on).abs() < 1e-6,
                "beat {beats}: expected gate={expected_on}, got {gate}",
            );
        }
    }
}
