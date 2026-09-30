//! `node.matter_state` — the substep boundary of a matter domain
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` D7, D8, D14). It holds the point state
//! across frames, provides the grid arrays, and has the executor repeat its
//! region `ticks × substeps_per_tick` times with six per-iteration scalars.
//! A non-finite tick (read back one frame late through a fenced ring) halts
//! the solver with a node error until the domain's epoch changes (Reset).

use manifold_gpu::GpuBuffer;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::matter::{
    ACCUM_WORDS_PER_NODE, MatterGridNode, MatterPoint, MatterTickStats, REACTION_WORDS, STATS_WORDS,
};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};

/// `reaction_in` closes node.matter_body_reaction into the region, so the
/// coupling sum runs every substep; the words themselves live in the domain's
/// reaction slot, which the domain reads back.
const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts { capture: "stats_in", output: "stats" },
    SubstepResultPorts { capture: "reaction_in", output: "reaction" },
];

/// The region's contract. Iteration scalars, in order: substep length in
/// seconds, iteration index this frame, substep within its tick, 1 on a
/// tick's first substep, 1 on its last, and the tick's index in the epoch.
pub const MATTER_STATE_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &[
        "step_dt",
        "step_index",
        "substep_in_tick",
        "tick_start",
        "tick_end",
        "tick_index",
    ],
    results: RESULTS,
    // The domain: offline it exchanges with Box3D between ticks.
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
    name: MatterState,
    type_id: "node.matter_state",
    purpose: "Hold a matter domain's points across frames and run its substep region: seed the points when the epoch changes, provide the grid arrays for the lattice, and repeat the region ticks × substeps-per-tick times with the substep length, indices and tick flags. A tick with non-finite values halts the solver with an error until Reset.",
    inputs: {
        seed: Array(MatterPoint) required,
        in: Array(MatterPoint) required,
        stats_in: Array(u32) required,
        reaction_in: Array(i32) required,
        count: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        ticks: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(MatterPoint),
        stats: Array(u32),
        reaction: Array(i32),
        grid_accum: Array(i32),
        grid: Array(MatterGridNode),
        step_dt: ScalarF32,
        step_index: ScalarF32,
        substep_in_tick: ScalarF32,
        tick_start: ScalarF32,
        tick_end: ScalarF32,
        tick_index: ScalarF32,
        live_count: ScalarF32,
        fault: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The substep boundary of the Live Matter group. seed and count come from node.matter_fill; lattice size, ticks, substeps_per_tick and epoch from node.matter_domain. The region body (zero_array → matter_move_bodies → matter_to_grid → matter_grid_update → matter_body_reaction → grid_to_matter → matter_stats) reads out, grid_accum and grid in place and closes back into in, stats_in and reaction_in (node.matter_body_reaction's reaction_out). out and stats escape to node.matter_frame; reaction is the last substep's reaction words, which nothing needs to read. Only live points (id ≠ 0) move.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"],
    picker: { label: "Matter State", category: Atom },
    summary: "Keeps the liquid's particles between frames and runs its simulation steps.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter state", "mpm state", "substeps"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        epoch: Option<u32> = None,
        pending: u32 = 0,
        substeps: u32 = 1,
        step_dt: f32 = 0.0,
        ticks_done: u64 = 0,
        captures: u32 = 0,
        grid_accum: Option<GpuBuffer> = None,
        grid: Option<GpuBuffer> = None,
        zero_stats: Option<GpuBuffer> = None,
        readback: Vec<ReadbackSlot> = Vec::new(),
        faulted: bool = false,
        last_stats: Option<MatterTickStats> = None,
    },
}

impl MatterState {
    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(&mut self, clock: Option<&manifold_gpu::FrameClock>) {
        let Some(epoch) = self.epoch else { return };
        let mut newest: Option<(u64, MatterTickStats)> = None;
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
            let words = unsafe {
                std::slice::from_raw_parts(ptr.cast::<u32>().cast_const(), STATS_WORDS as usize)
            };
            let stats = MatterTickStats::from_words(words);
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

impl Primitive for MatterState {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in", "stats_in", "reaction_in"]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &["out", "stats"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(MATTER_STATE_PORTS)
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "grid_accum" | "grid")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "grid_accum" => self.grid_accum.as_ref(),
            "grid" => self.grid.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities.iter().find(|(p, _)| *p == "seed").map(|&(_, n)| n),
            "stats" => Some(STATS_WORDS),
            "reaction" => Some(MAX_FLUID_ROLES as u32 * REACTION_WORDS),
            // Provided storage, sized to the lattice at run time.
            "grid_accum" | "grid" => Some(1),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let count = whole(ctx.scalar_or_param("count", 0.0));
        let nodes = [
            whole(ctx.scalar_or_param("nodes_x", 71.0)),
            whole(ctx.scalar_or_param("nodes_y", 71.0)),
            whole(ctx.scalar_or_param("nodes_z", 71.0)),
        ];
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        let substeps = whole(ctx.scalar_or_param("substeps_per_tick", 1.0)).max(1);
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let node_count = u64::from(nodes[0]) * u64::from(nodes[1]) * u64::from(nodes[2]);
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();

        let accum_bytes = node_count * u64::from(ACCUM_WORDS_PER_NODE) * 4;
        let grid_bytes = node_count * std::mem::size_of::<MatterGridNode>() as u64;
        if self.grid_accum.as_ref().is_none_or(|b| b.size < accum_bytes) {
            self.grid_accum = Some(gpu.device.create_buffer(accum_bytes.max(16)));
        }
        if self.grid.as_ref().is_none_or(|b| b.size < grid_bytes) {
            self.grid = Some(gpu.device.create_buffer(grid_bytes.max(32)));
        }
        let zero_stats = self
            .zero_stats
            .get_or_insert_with(|| gpu.device.create_buffer(u64::from(STATS_WORDS) * 4));

        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.ticks_done = 0;
            self.faulted = false;
            self.last_stats = None;
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * std::mem::size_of::<MatterPoint>() as u64)
                    .min(seed.size)
                    .min(out.size);
                if bytes > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                }
            }
            if let Some(stats) = stats {
                gpu.native_enc.clear_buffer(zero_stats);
                gpu.native_enc.copy_buffer_to_buffer(zero_stats, stats, u64::from(STATS_WORDS) * 4);
            }
        }
        self.poll_readbacks(clock.as_ref());

        self.substeps = substeps;
        self.step_dt = (TICK / f64::from(substeps)) as f32;
        self.pending = if self.faulted { 0 } else { ticks.saturating_mul(substeps) };
        self.captures = 0;
        let live = self.last_stats.map_or(count, |s| s.live);
        ctx.outputs.set_scalar("live_count", ParamValue::Float(live as f32));
        ctx.outputs.set_scalar("fault", ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }));
        if self.faulted {
            ctx.error("Matter: a tick produced non-finite values; the liquid is halted until Reset");
        }
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        let sub = iteration % self.substeps;
        let tick = self.ticks_done + u64::from(iteration / self.substeps);
        scalars[0] = self.step_dt;
        scalars[1] = iteration as f32;
        scalars[2] = sub as f32;
        scalars[3] = if sub == 0 { 1.0 } else { 0.0 };
        scalars[4] = if sub + 1 == self.substeps { 1.0 } else { 0.0 };
        scalars[5] = tick as f32;
        true
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // An in-place body already wrote `out` and `stats`; a body that
        // writes fresh storage is accepted by copy.
        let pairs = [("in", "out"), ("stats_in", "stats")];
        for (candidate, state) in pairs {
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
        // The reaction words stay in the domain's slot for its fenced
        // readback; the result carries only the region's final sums.
        if let (Some(candidate), Some(state)) = (ctx.inputs.array("reaction_in"), ctx.outputs.array("reaction"))
            && !candidate.ptr_eq(state)
        {
            let size = candidate.size.min(state.size);
            ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, state, size);
        }
        self.ticks_done += u64::from(self.pending / self.substeps);
        let Some(stats) = ctx.outputs.array("stats") else { return };
        let Some(epoch) = self.epoch else { return };
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let complete = |slot: &ReadbackSlot| !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp));
        let index = match self.readback.iter().position(complete) {
            Some(index) => index,
            None if self.readback.len() < READBACK_SLOTS => {
                self.readback.push(ReadbackSlot {
                    buffer: gpu.device.create_buffer_shared(u64::from(STATS_WORDS) * 4),
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
        if slot.pending {
            // Retired but unread: poll will not see it again, so read it now.
            slot.pending = false;
        }
        gpu.native_enc.copy_buffer_to_buffer(stats, &slot.buffer, u64::from(STATS_WORDS) * 4);
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
    fn matter_state_serves_six_iteration_scalars_per_substep() {
        let mut state = MatterState::new();
        state.pending = 2 * 3;
        state.substeps = 3;
        state.step_dt = 0.5;
        state.ticks_done = 10;
        let mut s = [0.0f32; 6];
        let mut seen = Vec::new();
        for i in 0.. {
            if !EffectNode::substep_iteration(&mut state, i, &mut s) {
                break;
            }
            seen.push(s);
        }
        assert_eq!(seen.len(), 6);
        assert_eq!(seen[0], [0.5, 0.0, 0.0, 1.0, 0.0, 10.0]);
        assert_eq!(seen[2], [0.5, 2.0, 2.0, 0.0, 1.0, 10.0]);
        assert_eq!(seen[3], [0.5, 3.0, 0.0, 1.0, 0.0, 11.0]);
        assert_eq!(MATTER_STATE_PORTS.iteration_scalars.len(), 6);
    }
}
