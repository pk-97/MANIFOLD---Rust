//! `node.lfo` — beat-locked low-frequency oscillator emitting a
//! scalar in `[0, 1]` on the `out` port.
//!
//! Stateless: phase is `(beats * rate + offset).fract()` computed
//! fresh each frame from [`FrameTime::beats`]. Seek-safe and
//! deterministic — pause/resume returns to the same value, and two
//! graphs at the same transport position emit identical phases.
//!
//! The `rate` selector reuses the same note-rate table as
//! `node.strobe` so the editor's musical-rate vocabulary stays
//! consistent across the catalog. `phase` is a fractional offset on
//! `[0, 1)` for layering multiple LFOs out-of-phase. Output is
//! unipolar `[0, 1]` for direct wiring into `[0, 1]`-ranged knobs
//! like `wet_dry`; bipolar shaping (depth, bias) composes through
//! `node.math` downstream.

use std::f32::consts::TAU;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::primitives::strobe::NOTE_RATE_VALUES;

/// Display labels for the `rate` enum. Indices match
/// [`NOTE_RATE_VALUES`] from `node.strobe` — same musical vocabulary.
pub const LFO_RATE_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64",
];

/// Display labels for the `shape` enum, indexed by enum value.
pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];

crate::primitive! {
    name: Lfo,
    type_id: "node.lfo",
    purpose: "Beat-synced low-frequency oscillator. Emits a unipolar [0, 1] scalar on `out`, shaped sine / triangle / saw / square at a musical note rate. Stateless and seek-safe.",
    inputs: {},
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: "rate",
            label: "Rate",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // "1/4"
            range: Some((0.0, (LFO_RATE_LABELS.len() - 1) as f32)),
            enum_values: LFO_RATE_LABELS,
        },
        ParamDef {
            name: "shape",
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (LFO_SHAPES.len() - 1) as f32)),
            enum_values: LFO_SHAPES,
        },
        ParamDef {
            name: "phase",
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output range is unipolar [0, 1]. For depth/offset or bipolar shaping, wire the output through `node.math`. For free-running (non-beat-locked) rate, use `node.value` or a future `node.time` source through math.",
    examples: [],
    picker: { label: "LFO", category: Driver },
}

impl Primitive for Lfo {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let rate_idx = match ctx.params.get("rate") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(NOTE_RATE_VALUES.len() - 1),
            Some(ParamValue::Int(i)) => ((*i).max(0) as usize).min(NOTE_RATE_VALUES.len() - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as usize).min(NOTE_RATE_VALUES.len() - 1)
            }
            _ => 2,
        };
        let shape = match ctx.params.get("shape") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(LFO_SHAPES.len() - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as usize).min(LFO_SHAPES.len() - 1)
            }
            _ => 0,
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
        let value = match shape {
            0 => 0.5 * (1.0 + (TAU * p).sin()),
            1 => {
                if p < 0.5 {
                    2.0 * p
                } else {
                    2.0 - 2.0 * p
                }
            }
            2 => p,
            _ => {
                if p < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
        };

        ctx.outputs.set_scalar("out", ParamValue::Float(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::backend::Backend;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeType, FrameTime, NodeInstanceId,
    };
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

    fn drive_lfo(rate_idx: u32, shape_idx: u32, phase: f32, beats: f32) -> f32 {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let lfo = g.add_node(Box::new(Lfo::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(lfo, "rate", ParamValue::Enum(rate_idx)).unwrap();
        g.set_param(lfo, "shape", ParamValue::Enum(shape_idx)).unwrap();
        g.set_param(lfo, "phase", ParamValue::Float(phase)).unwrap();
        g.connect((lfo, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_at_beats(beats));
        let v = *seen.lock().unwrap();
        match v {
            Some(ParamValue::Float(f)) => f,
            _ => panic!("LFO did not emit a Float value: {v:?}"),
        }
    }

    #[test]
    fn sine_lfo_at_phase_zero_returns_half() {
        // rate=2 (1/4), shape=0 (Sine), phase=0, beats=0 → sin(0) → 0.5 unipolar.
        let v = drive_lfo(2, 0, 0.0, 0.0);
        assert!((v - 0.5).abs() < 1e-4, "sine(0) unipolar = 0.5, got {v}");
    }

    #[test]
    fn saw_lfo_quarter_at_beat_quarter() {
        // rate=2 (1/4 → 1 cycle per beat), shape=2 (Saw), phase=0, beats=0.25 → 0.25.
        let v = drive_lfo(2, 2, 0.0, 0.25);
        assert!((v - 0.25).abs() < 1e-4, "saw at 0.25 phase = 0.25, got {v}");
    }

    #[test]
    fn square_lfo_low_in_first_half() {
        let v_low = drive_lfo(2, 3, 0.0, 0.1);
        let v_high = drive_lfo(2, 3, 0.0, 0.7);
        assert!(v_low < 0.01 && v_high > 0.99, "square: low={v_low}, high={v_high}");
    }

    #[test]
    fn triangle_lfo_peaks_at_half() {
        // Saw goes 0→1 across the cycle; triangle goes 0→1→0.
        let v_peak = drive_lfo(2, 1, 0.0, 0.5);
        let v_zero = drive_lfo(2, 1, 0.0, 0.0);
        assert!(
            (v_peak - 1.0).abs() < 1e-4 && v_zero.abs() < 1e-4,
            "triangle: peak@0.5={v_peak}, zero@0={v_zero}"
        );
    }

    #[test]
    fn phase_offset_shifts_waveform() {
        // Saw with phase=0.25 at beats=0 should emit 0.25 — same as
        // beats=0.25 with phase=0.
        let v = drive_lfo(2, 2, 0.25, 0.0);
        assert!((v - 0.25).abs() < 1e-4, "saw phase=0.25 at beats=0 should be 0.25, got {v}");
        let _ = NodeInstanceId(0);
        let _: &dyn Backend = &crate::node_graph::backend::MockBackend::new();
    }
}
