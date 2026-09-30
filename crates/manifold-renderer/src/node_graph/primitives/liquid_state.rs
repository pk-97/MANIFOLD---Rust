//! `node.liquid_state` — the tick boundary of a particle liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D10): it holds the particles across
//! frames and has the executor run its region once per liquid tick the
//! domain's clock is due. The region body is one whole tick. A non-finite
//! tick (read back one frame late through a fenced ring) halts the liquid
//! with a node error until the domain's epoch changes (Reset).

use manifold_gpu::GpuBuffer;

use super::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};

const RESULTS: &[SubstepResultPorts] = &[SubstepResultPorts { capture: "stats_in", output: "stats" }];

/// The region's contract. The one iteration scalar is the tick's index in
/// the epoch.
pub const LIQUID_STATE_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &["tick_index"],
    results: RESULTS,
    // The domain owns the clock; offline it may sync between ticks.
    clock: Some("ticks"),
};

/// Stats readbacks in flight: the GPU writes a slot at the end of a frame's
/// region, the CPU reads it once the frame clock says that frame retired.
const READBACK_SLOTS: usize = 3;

pub struct ReadbackSlot {
    buffer: GpuBuffer,
    stamp: u64,
    epoch: u32,
    pending: bool,
}

crate::primitive! {
    name: LiquidState,
    type_id: "node.liquid_state",
    purpose: "Hold a particle liquid across frames and run its tick region: seed the particles when the epoch changes, then repeat the region once per due tick with the tick's index. The region's last particles become the state, and its last stats escape with them. A tick with non-finite values halts the liquid with an error until Reset.",
    inputs: {
        seed: Array(FluidParticle) required,
        in: Array(FluidParticle) required,
        stats_in: Array(u32) required,
        count: ScalarF32 optional,
        ticks: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        stats: Array(u32),
        tick_index: ScalarF32,
        live_count: ScalarF32,
        fault: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The tick boundary of a particle liquid. seed and count come from the fill; ticks and epoch from the liquid's domain (the region's clock owner). The body is one tick: every step of the solver from out, then node.liquid_stats over the tick's last particles, closing back into in and stats_in. out and stats escape to node.liquid_frame.",
    examples: [],
    picker: { label: "Liquid State", category: Atom },
    summary: "Keeps a particle liquid between frames and runs one pass of its simulation per tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid state", "tick loop", "particle state"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        epoch: Option<u32> = None,
        pending: u32 = 0,
        ticks_done: u64 = 0,
        captures: u32 = 0,
        zero_stats: Option<GpuBuffer> = None,
        readback: Vec<ReadbackSlot> = Vec::new(),
        faulted: bool = false,
        last_stats: Option<LiquidTickStats> = None,
    },
}

impl LiquidState {
    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(&mut self, clock: Option<&manifold_gpu::FrameClock>) {
        let Some(epoch) = self.epoch else { return };
        let mut newest: Option<(u64, LiquidTickStats)> = None;
        for slot in self.readback.iter_mut().filter(|s| s.pending) {
            if !clock.is_none_or(|c| c.is_complete(slot.stamp)) {
                continue;
            }
            slot.pending = false;
            if slot.epoch != epoch {
                continue;
            }
            let Some(ptr) = slot.buffer.mapped_ptr() else { continue };
            // SAFETY: the frame that wrote this shared slot has retired.
            let words =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u32>().cast_const(), LIQUID_STATS_WORDS as usize) };
            let stats = LiquidTickStats::from_words(words);
            if newest.is_none_or(|(stamp, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, stats));
            }
        }
        if let Some((_, stats)) = newest {
            if stats.nonfinite > 0 {
                self.faulted = true;
            }
            self.last_stats = Some(stats);
        }
    }
}

impl Primitive for LiquidState {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in", "stats_in"]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &["out", "stats"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(LIQUID_STATE_PORTS)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities.iter().find(|(p, _)| *p == "seed").map(|&(_, n)| n),
            "stats" => Some(LIQUID_STATS_WORDS),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let count = whole(ctx.scalar_or_param("count", 0.0));
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let zero_stats = self.zero_stats.get_or_insert_with(|| gpu.device.create_buffer(stats_bytes));

        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.ticks_done = 0;
            self.faulted = false;
            self.last_stats = None;
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * std::mem::size_of::<FluidParticle>() as u64).min(seed.size).min(out.size);
                if bytes > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                }
            }
            if let Some(stats) = stats {
                gpu.native_enc.clear_buffer(zero_stats);
                gpu.native_enc.copy_buffer_to_buffer(zero_stats, stats, stats_bytes.min(stats.size));
            }
        }
        self.poll_readbacks(clock.as_ref());

        self.pending = if self.faulted { 0 } else { ticks };
        self.captures = 0;
        let live = self.last_stats.map_or(count, |s| s.live);
        ctx.outputs.set_scalar("live_count", ParamValue::Float(live as f32));
        ctx.outputs.set_scalar("fault", ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }));
        if self.faulted {
            ctx.error("Liquid State: a tick produced non-finite values; the liquid is halted until Reset");
        }
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        scalars[0] = (self.ticks_done + u64::from(iteration)) as f32;
        true
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A body that writes fresh storage is accepted by copy.
        for (candidate, state) in [("in", "out"), ("stats_in", "stats")] {
            if let (Some(candidate), Some(state)) = (ctx.inputs.array(candidate), ctx.outputs.array(state))
                && !candidate.ptr_eq(state)
            {
                let size = candidate.size.min(state.size);
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, state, size);
            }
        }
        self.captures += 1;
        if self.captures != self.pending {
            return;
        }
        self.ticks_done += u64::from(self.pending);
        let Some(stats) = ctx.outputs.array("stats") else { return };
        let Some(epoch) = self.epoch else { return };
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let complete = |slot: &ReadbackSlot| !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp));
        let index = match self.readback.iter().position(complete) {
            Some(index) => index,
            None if self.readback.len() < READBACK_SLOTS => {
                self.readback.push(ReadbackSlot {
                    buffer: gpu.device.create_buffer_shared(stats_bytes),
                    stamp: 0,
                    epoch,
                    pending: false,
                });
                self.readback.len() - 1
            }
            // Every slot is still in flight: skip this frame's readback; live
            // never waits on the GPU.
            None => return,
        };
        let slot = &mut self.readback[index];
        gpu.native_enc.copy_buffer_to_buffer(stats, &slot.buffer, stats_bytes.min(stats.size));
        slot.stamp = clock.as_ref().map_or(0, |c| c.stamp());
        slot.epoch = epoch;
        slot.pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn liquid_state_serves_the_tick_index_per_iteration() {
        let mut state = LiquidState::new();
        state.pending = 3;
        state.ticks_done = 10;
        let mut scalars = [0.0f32; 1];
        let mut seen = Vec::new();
        for i in 0.. {
            if !EffectNode::substep_iteration(&mut state, i, &mut scalars) {
                break;
            }
            seen.push(scalars[0]);
        }
        assert_eq!(seen, [10.0, 11.0, 12.0]);
        assert_eq!(LIQUID_STATE_PORTS.iteration_scalars.len(), 1);
    }
}
