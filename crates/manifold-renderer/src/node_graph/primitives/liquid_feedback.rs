//! `node.liquid_feedback` — one-frame delay for liquid particles
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3): the particles a frame's last water
//! step wrote become the next frame's input. `node.array_feedback` for
//! `Array(FluidParticle)`; both share its emit and capture.

use super::array_feedback::{capture_for_next_frame, emit_delayed};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: LiquidFeedback,
    type_id: "node.liquid_feedback",
    purpose: "One-frame delay for liquid particles: this frame's `in` becomes next frame's `out`. On the first frame `out` is `seed`; when `reset_trigger`'s whole value changes, the next frame starts again from `seed`.",
    inputs: {
        in: Array(FluidParticle) required,
        seed: Array(FluidParticle) required,
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Closes a particle liquid's frame loop without a graph cycle: seed from node.liquid_fill, out into the frame's first water step, the last step's particles back into in. Wire the generator's trigger count or a reset control into reset_trigger to start the liquid over.",
    examples: [],
    picker: { label: "Liquid Feedback", category: Atom },
    summary: "Keeps the liquid's particles from one frame to the next.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid state", "particle feedback", "frame delay"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        last_reset_trigger: Option<i32> = None,
    },
}

impl Primitive for LiquidFeedback {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in"]
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "seed").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        emit_delayed(ctx, &mut self.last_reset_trigger);
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        capture_for_next_frame(ctx);
    }
}

// The one-frame delay copies whole records; this pins the record it copies.
const _: () = assert!(std::mem::size_of::<FluidParticle>() == 32);
