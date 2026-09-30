//! `node.field_feedback` — one-frame delay for a float array that starts
//! from zeros: the FFT water solve's warm start carries each solve's collar
//! sources into the next frame's first step through it
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3). `node.array_feedback` for
//! `Array(f32)`; they share its emit and capture.

use std::borrow::Cow;

use super::array_feedback::{DelayStart, capture_for_next_frame, emit_delayed};
use super::sort_particles_into_cells::int_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: FieldFeedback,
    type_id: "node.field_feedback",
    purpose: "One-frame delay for an Array<f32> of max_capacity floats: this frame's `in` becomes next frame's `out`. `out` is all zeros on the first frame, after the node's state is cleared (seek, project load, restart), and on the frame after `reset_trigger`'s whole value changes.",
    inputs: {
        in: Array(f32) required,
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        int_param!("max_capacity", "Max Floats", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Carries a per-frame float field forward without a graph cycle, starting from nothing: out into the first consumer of the frame, the frame's last producer back into in. The FFT water solve warm-starts each frame's first solve from the last frame's collar sources this way; wire the same reset as the liquid's node.liquid_feedback so both start over together.",
    examples: [],
    picker: { label: "Field Feedback", category: Atom },
    summary: "Keeps a list of numbers from one frame to the next, starting from zero.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["float feedback", "field delay", "frame delay", "warm start"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        last_reset_trigger: Option<i32> = None,
    },
}

fn max_capacity(params: &ParamValues) -> u32 {
    match params.get("max_capacity") {
        Some(ParamValue::Float(v)) => v.round().max(1.0) as u32,
        _ => 1,
    }
}

impl Primitive for FieldFeedback {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in"]
    }

    /// Sized from the param: `in` is the back edge, produced later in the
    /// frame, so its size is not known when `out` is planned.
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| max_capacity(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        emit_delayed(ctx, &mut self.last_reset_trigger, DelayStart::Zeros);
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        capture_for_next_frame(ctx);
    }
}
