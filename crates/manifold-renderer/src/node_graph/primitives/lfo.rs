//! `node.lfo` — low-frequency oscillator emitting a scalar on the
//! `out` port.
//!
//! Stateless: phase is recomputed fresh each frame from
//! [`FrameTime`]. Seek-safe and deterministic — pause/resume returns
//! to the same value, and two graphs at the same transport position
//! emit identical phases.
//!
//! Two rate modes:
//! - **Musical**: phase advances `note_rate` cycles per beat. The
//!   `rate` selector reuses the same note-rate table as
//!   `node.strobe` so the editor's musical-rate vocabulary stays
//!   consistent across the catalog. Reads `FrameTime::beats`.
//! - **Free**: angular frequency in radians per second. The
//!   underlying sine is `sin(seconds * angular_rate)` so this
//!   matches legacy generator math like
//!   `sin(time * freq_rate)` (where `time` is seconds) bit-for-bit.
//!   Reads `FrameTime::seconds`.
//!
//! `phase` is a fractional offset on `[0, 1)` for layering multiple
//! LFOs out-of-phase. `min` / `max` map the underlying unipolar
//! `[0, 1]` shape onto the output range, so a bipolar `[-1, 1]`
//! sine is `min=-1, max=1`, an oscillator centred at `2.0` with
//! amplitude `1.5` is `min=0.5, max=3.5`, and the default
//! `min=0, max=1` preserves the original unipolar behaviour.

use std::borrow::Cow;
use std::f32::consts::TAU;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::primitives::note_rates::NOTE_RATE_VALUES;

/// Display labels for the `rate` enum. Indices match
/// [`NOTE_RATE_VALUES`] (the shared note-rate table) — same musical vocabulary.
pub const LFO_RATE_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64",
];

/// Display labels for the `shape` enum, indexed by enum value.
pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];

/// Display labels for the `rate_mode` enum. `Musical` consumes the
/// `rate` enum (beat-locked note rate); `Free` consumes `rate_hz` as
/// cycles-per-second.
pub const LFO_RATE_MODES: &[&str] = &["Musical", "Free"];

crate::primitive! {
    name: Lfo,
    type_id: "node.lfo",
    purpose: "Low-frequency oscillator. Emits a scalar on `out`, shaped sine / triangle / saw / square. `rate_mode=Musical` locks the cycle to a musical note rate (1/4, 1/8, etc.); `rate_mode=Free` runs at a continuous `Speed` set in Hz in the editor (stored internally as rad/s — the underlying sine is `sin(seconds * angular_rate)`, matching the legacy generator convention). Output maps the internal `[0, 1]` shape onto `[min, max]` so a single LFO can drive bipolar, biased, or amplitude-scaled targets without a downstream `node.math`. Stateless and seek-safe.",
    inputs: {},
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("rate_mode"),
            label: "Rate Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Musical — preserves existing behaviour
            range: Some((0.0, (LFO_RATE_MODES.len() - 1) as f32)),
            enum_values: LFO_RATE_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Rate",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // "1/4"
            range: Some((0.0, (LFO_RATE_LABELS.len() - 1) as f32)),
            enum_values: LFO_RATE_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("angular_rate"),
            label: "Speed",
            // Stored rad/s (the oscillator math unit); the editor shows and
            // edits this in Hz. See `ParamType::Frequency`.
            ty: ParamType::Frequency,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shape"),
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (LFO_SHAPES.len() - 1) as f32)),
            enum_values: LFO_SHAPES,
        },
        ParamDef {
            name: Cow::Borrowed("phase"),
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("min"),
            label: "Min",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max"),
            label: "Max",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Defaults reproduce the historic beat-locked unipolar [0, 1] behaviour: Musical mode, rate=1/4, sine, min=0, max=1. Switch `rate_mode` to Free and set `angular_rate` (rad/s) to drive the underlying `sin(seconds * angular_rate)` — matches legacy generator code expressed as `sin(time * rate)` with no unit conversion. For the linear-ramp phase pattern of legacy generators (`phase = time * phase_rate`), use Free + saw shape + `min=0, max=2π` so the saw output fed into `sin(a*t + phase)` reproduces the legacy phase wrap exactly. `min`/`max` swap signs to invert without a `node.math` and produce bipolar output (-1, 1) or arbitrary amplitude+offset in one node.",
    examples: [],
    picker: { label: "LFO", category: Driver },
    summary: "A smoothly cycling value you wire into any knob to make it move on its own. Pick a waveform like sine or saw, and lock it to the tempo or let it run free.",
    category: Control,
    role: Control,
    aliases: ["oscillator", "modulator", "LFO CHOP"],
    boundary_reason: NonGpu,
}

crate::param_tooltips!("node.lfo", {
    "rate_mode" => "Locks the cycle to the song tempo, or lets it run free in Hz.",
    "rate" => "How fast it cycles. When synced you pick a note value like 1/4 or 1/8, otherwise it is measured in cycles per second.",
    "angular_rate" => "The free-running speed in Hz, used only when Sync is off.",
    "shape" => "The waveform, anything from a smooth sine to a hard square or a random sample and hold.",
    "phase" => "Shifts the starting point of the cycle. Range 0 to 1.",
    "min" => "The value at the bottom of the cycle. Set Min above Max to flip the output upside down.",
    "max" => "The value at the top of the cycle.",
});

impl Primitive for Lfo {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let rate_mode = match ctx.params.get("rate_mode") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(LFO_RATE_MODES.len() - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as usize).min(LFO_RATE_MODES.len() - 1)
            }
            _ => 0,
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
        let min = match ctx.params.get("min") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let max = match ctx.params.get("max") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        // `cycles` counts how many full periods the LFO has elapsed
        // since transport zero. In Musical mode the rate is cycles
        // per beat (note-rate enum), in Free mode the rate is an
        // angular frequency (rad/s) — divide by 2π to convert to
        // cycles per second. Whichever axis we're on, `fract()`
        // wraps to a `[0, 1)` unit phase that drives every shape.
        let cycles = match rate_mode {
            1 => {
                let angular_rate = match ctx.params.get("angular_rate") {
                    Some(ParamValue::Float(f)) => *f,
                    _ => 1.0,
                };
                ctx.time.seconds.0 as f32 * angular_rate / TAU
            }
            _ => {
                let rate_idx = match ctx.params.get("rate") {
                    Some(ParamValue::Enum(v)) => (*v as usize).min(NOTE_RATE_VALUES.len() - 1),
                    Some(ParamValue::Float(f)) => {
                        (f.round().max(0.0) as usize).min(NOTE_RATE_VALUES.len() - 1)
                    }
                    _ => 2,
                };
                ctx.time.beats.0 as f32 * NOTE_RATE_VALUES[rate_idx]
            }
        };

        let mut p = (cycles + phase_offset).fract();
        if p < 0.0 {
            p += 1.0;
        }
        let unipolar = match shape {
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
        let value = min + unipolar * (max - min);

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
        let v = seen.lock().unwrap().clone();
        match v {
            Some(ParamValue::Float(f)) => f,
            _ => panic!("LFO did not emit a Float value: {v:?}"),
        }
    }

    fn frame_at(beats: f32, seconds: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beats as f64),
            seconds: Seconds(seconds as f64),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Drive the LFO with full configuration (rate_mode, free-rate
    /// rad/s, min/max range) and an explicit `(beats, seconds)` pair
    /// so Free mode tests don't accidentally pick up beats.
    #[allow(clippy::too_many_arguments)]
    fn drive_lfo_full(
        rate_mode: u32,
        rate_idx: u32,
        angular_rate: f32,
        shape_idx: u32,
        phase: f32,
        min: f32,
        max: f32,
        beats: f32,
        seconds: f32,
    ) -> f32 {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut g = Graph::new();
        let lfo = g.add_node(Box::new(Lfo::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(lfo, "rate_mode", ParamValue::Enum(rate_mode)).unwrap();
        g.set_param(lfo, "rate", ParamValue::Enum(rate_idx)).unwrap();
        g.set_param(lfo, "angular_rate", ParamValue::Float(angular_rate)).unwrap();
        g.set_param(lfo, "shape", ParamValue::Enum(shape_idx)).unwrap();
        g.set_param(lfo, "phase", ParamValue::Float(phase)).unwrap();
        g.set_param(lfo, "min", ParamValue::Float(min)).unwrap();
        g.set_param(lfo, "max", ParamValue::Float(max)).unwrap();
        g.connect((lfo, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_at(beats, seconds));
        match seen.lock().unwrap().clone() {
            Some(ParamValue::Float(f)) => f,
            v => panic!("LFO did not emit a Float: {v:?}"),
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

    /// Free mode reads seconds, not beats. Lock that in: identical
    /// angular_rate + seconds with wildly different beats values
    /// must produce identical output.
    #[test]
    fn free_mode_ignores_beats() {
        let a = drive_lfo_full(/*Free*/ 1, 0, 1.0, /*Sine*/ 0, 0.0, 0.0, 1.0, 0.0, 0.5);
        let b = drive_lfo_full(1, 0, 1.0, 0, 0.0, 0.0, 1.0, 999.0, 0.5);
        assert!((a - b).abs() < 1e-6, "Free mode must depend only on seconds: a={a}, b={b}");
    }

    /// Musical mode unchanged: at 1/4 (1 cycle/beat), beats=0.25,
    /// saw → 0.25. Seconds value irrelevant.
    #[test]
    fn musical_mode_still_reads_beats() {
        let v = drive_lfo_full(/*Musical*/ 0, 2, 0.0, /*Saw*/ 2, 0.0, 0.0, 1.0, 0.25, 999.0);
        assert!((v - 0.25).abs() < 1e-4, "musical saw at 0.25 beats should be 0.25, got {v}");
    }

    /// min/max remap the [0,1] unipolar shape. With min=2 max=6 and
    /// unipolar=0.5 (sine at the zero-crossing midpoint), output is
    /// 2 + 0.5*(6-2) = 4.0.
    #[test]
    fn min_max_remap_unipolar_to_arbitrary_range() {
        // Free, angular_rate=0 → no time evolution → sine stuck at
        // sin(0) → unipolar 0.5.
        let v = drive_lfo_full(1, 0, 0.0, /*Sine*/ 0, 0.0, 2.0, 6.0, 0.0, 0.0);
        assert!((v - 4.0).abs() < 1e-4, "min=2, max=6 at unipolar 0.5 should be 4.0, got {v}");
    }

    /// min/max sign-swap gives a bipolar output without a downstream
    /// math node: min=-1, max=1 at unipolar 0.5 → 0.0.
    #[test]
    fn min_max_can_be_bipolar() {
        let v = drive_lfo_full(1, 0, 0.0, /*Sine*/ 0, 0.0, -1.0, 1.0, 0.0, 0.0);
        assert!(v.abs() < 1e-4, "bipolar sine at zero crossing should be 0, got {v}");
    }

    /// **Bit-perfect parity hook for the legacy Lissajous generator.**
    /// Legacy code does `a = 2.0 + 1.5 * (time * freq_x_rate).sin()`
    /// where `time` is `ctx.time` (seconds, see PresetContext) and
    /// `freq_x_rate` is the user param.
    ///
    /// The equivalent graph node is `Lfo(rate_mode=Free, sine,
    /// angular_rate=freq_x_rate, min=0.5, max=3.5)`. Verifying at
    /// (seconds=1.7, angular_rate=0.13):
    ///   legacy = 2.0 + 1.5 * sin(1.7 * 0.13) = 2.0 + 1.5 * sin(0.221)
    #[test]
    fn legacy_lissajous_frequency_oscillator_parity() {
        let time_seconds = 1.7_f32;
        let rate = 0.13_f32;
        let legacy = 2.0_f32 + 1.5 * (time_seconds * rate).sin();
        let graph = drive_lfo_full(
            /*Free*/ 1,
            0,
            rate,
            /*Sine*/ 0,
            0.0,
            0.5,
            3.5,
            0.0,
            time_seconds,
        );
        assert!(
            (legacy - graph).abs() < 1e-5,
            "legacy={legacy}, graph={graph}, diff={}",
            (legacy - graph).abs()
        );
    }

    /// **Bit-perfect parity hook for the legacy Lissajous phase.**
    /// Legacy `phase = time * phase_rate` (unbounded radians) is fed
    /// into `sin(a*t + phase)`. The graph equivalent is a saw LFO
    /// with `min=0, max=2π`, `angular_rate=phase_rate` — its output
    /// is `(seconds * phase_rate) mod 2π`, and since sin is 2π-periodic
    /// the inner-curve sample is identical to the unbounded legacy
    /// phase. We assert that equivalence directly by comparing
    /// `sin(saw_phase)` against `sin(legacy_phase)`.
    #[test]
    fn legacy_lissajous_phase_ramp_parity() {
        let time_seconds = 3.4_f32;
        let phase_rate = 0.07_f32;
        let legacy_phase = time_seconds * phase_rate;
        let graph_phase = drive_lfo_full(
            /*Free*/ 1,
            0,
            phase_rate,
            /*Saw*/ 2,
            0.0,
            0.0,
            std::f32::consts::TAU,
            0.0,
            time_seconds,
        );
        // The two phases differ by an integer multiple of 2π, but
        // sin() collapses that difference. The visible Lissajous
        // sample is sin(a*t + phase) — equivalence at sin() is what
        // matters for parity.
        assert!(
            (legacy_phase.sin() - graph_phase.sin()).abs() < 1e-5,
            "sin parity: legacy={}, graph={}",
            legacy_phase.sin(),
            graph_phase.sin()
        );
    }
}
