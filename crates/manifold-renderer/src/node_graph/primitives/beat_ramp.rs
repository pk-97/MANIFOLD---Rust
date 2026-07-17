//! `node.beat_ramp` — per-beat attack envelope. Emits
//! `clamp(fract(beats·rate) / attack, 0, 1)` on its `out` scalar port:
//! a value that snaps to 0 at each beat and ramps to 1 over the first
//! `attack` fraction of the cycle, then holds.
//!
//! The musician-facing "pop-in" envelope — Voronoi Prism fades each
//! beat's cells in over the first 15% of the beat. Stateless and
//! seek-safe (reads `FrameTime.beats` fresh each frame, or a wired
//! `beat` scalar). Wire into any scalar input (gain, opacity, a mask
//! multiplier) for a beat-synced attack.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: BeatRamp,
    type_id: "node.beat_ramp",
    purpose: "Per-beat attack envelope: out = clamp(fract(beats·rate) / attack, 0, 1). Snaps to 0 each beat, ramps to 1 over the first `attack` fraction of the cycle, then holds. The musician-facing pop-in envelope (Voronoi Prism fades cells in over the first 15% of each beat). Stateless / seek-safe — reads FrameTime.beats (or a wired `beat`). Wire into any scalar (gain, mask multiplier, opacity) for a beat-synced attack.",
    inputs: {
        beat: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Rate (cycles/beat)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0625, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("attack"),
            label: "Attack",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.001, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "rate = cycles per beat (1 = one ramp per beat); attack = fraction of the cycle the ramp takes (0.15 = Voronoi Prism's pop-in). At attack→0 it's effectively a gate (instant on). The `beat` input port-shadows FrameTime.beats — unwired it reads the playback clock, matching node.beat_gate. Pure scalar, no GPU.",
    examples: ["preset.effect.voronoi_prism"],
    picker: { label: "Beat Ramp", category: Driver },
    summary: "Rises from 0 to 1 across each beat then snaps back, a sawtooth locked to the tempo. Wire it into anything you want to sweep in time with the music.",
    category: Control,
    role: Control,
    aliases: ["beat ramp", "saw", "tempo ramp", "phase"],
    boundary_reason: NonGpu,
}

impl Primitive for BeatRamp {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let rate = match ctx.params.get("rate") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let attack = match ctx.params.get("attack") {
            Some(ParamValue::Float(f)) => f.max(1e-5),
            _ => 0.15,
        };
        let beat = match ctx.inputs.scalar("beat") {
            Some(ParamValue::Float(f)) => f,
            _ => ctx.time.beats.0 as f32,
        };

        let mut phase = (beat * rate).fract();
        if phase < 0.0 {
            phase += 1.0;
        }
        let pop_in = (phase / attack).clamp(0.0, 1.0);
        ctx.outputs.set_scalar("out", ParamValue::Float(pop_in));
    }
}
