//! `node.matter_state` — the substep boundary of a matter domain
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` D7, D8, D14). It holds the point state
//! across frames, provides the grid arrays, and has the executor repeat its
//! region over each accepted interval's subdivisions with six iteration scalars.
//! A non-finite tick (read back one frame late through a fenced ring) halts
//! the solver with a node error until the domain's epoch changes (Reset).

use manifold_gpu::GpuBuffer;

use crate::exec::effect_node::EffectNodeContext;
use crate::water::fluid_role::MAX_FLUID_ROLES;
use crate::water::matter::{MatterGridNode, MatterPoint, MatterTickStats, REACTION_WORDS, STATS_WORDS, grid_accum_bytes, grid_bytes, substep_duration};
use crate::parameters::ParamValue;
use crate::water::physics_metrics::DroppedTimeTracker;
use crate::primitive::Primitive;
use crate::exec::substeps::{SubstepBoundaryPorts, SubstepInterval, SubstepResultPorts};

/// `reaction_in` closes the reaction chain (node.matter_body_reaction, then
/// node.grid_to_matter) into the region, so the coupling sum runs every
/// substep; the words themselves live in the domain's reaction slot, which
/// the domain reads back.
const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts { capture: "stats_in", output: "stats", optional: false },
    SubstepResultPorts { capture: "reaction_in", output: "reaction", optional: false },
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
    endpoint: f64,
    cap_hit: bool,
}

crate::primitive! {
    name: MatterState,
    type_id: "node.matter_state",
    purpose: "Hold a matter domain's points across frames and run its substep region: seed the points when the epoch changes, provide the grid arrays for the lattice, and repeat the region over each accepted interval's subdivisions with the substep length, indices and tick flags. A tick with non-finite values halts the solver with an error until Reset.",
    inputs: {
        seed: Array(MatterPoint) required,
        in: Array(MatterPoint) required,
        stats_in: Array(u32) required,
        reaction_in: Array(i32) required,
        count: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        ticks: ScalarF32 optional,
    interval_duration: ScalarF32 optional,
    simulation_time: ScalarF32 optional, target_time: ScalarF32 optional,
        dropped_seconds: ScalarF32 optional,
    step_cap_hit: ScalarF32 optional,
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
        interval: Option<SubstepInterval> = None,
        ticks_done: u64 = 0,
        captures: u32 = 0,
        captured_ticks: u32 = 0,
        grid_accum: Option<GpuBuffer> = None,
        grid: Option<GpuBuffer> = None,
        zero_stats: Option<GpuBuffer> = None,
        readback: Vec<ReadbackSlot> = Vec::new(),
        faulted: bool = false,
        submitted_time: f64 = 0.0,
        completed_time: f64 = 0.0,
        dropped_time: DroppedTimeTracker = DroppedTimeTracker::default(),
        submitted_cap: bool = false,
        completed_cap: bool = false,
        last_stats: Option<MatterTickStats> = None,
    },
}

impl MatterState {
    /// Count completed accepted intervals, rather than dividing a frame's
    /// iteration total by whichever interval's count happened to run last.
    fn capture_iteration(&mut self) -> bool {
        self.captures += 1;
        let tick_end = self.interval.map_or_else(
            || self.captures.is_multiple_of(self.substeps),
            |timing| self.captures == timing.first_iteration + timing.iterations,
        );
        if tick_end {
            self.captured_ticks += 1;
        }
        if self.captures == self.pending {
            self.ticks_done += u64::from(self.captured_ticks);
            true
        } else {
            false
        }
    }

    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(&mut self, clock: Option<&manifold_gpu::FrameClock>, live: bool) {
        let Some(epoch) = self.epoch else { return };
        let mut newest: Option<(u64, MatterTickStats, f64, bool)> = None;
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
            if newest.is_none_or(|(stamp, _, _, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, stats, slot.endpoint, slot.cap_hit));
            }
        }
        if let Some((_, stats, endpoint, cap_hit)) = newest {
            self.faulted = stats.nonfinite > 0 || (self.faulted && !live);
            self.completed_time = endpoint;
            self.completed_cap = cap_hit;
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
        _params: &crate::exec::effect_node::ParamValues,
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
        let live_mode = !crate::water::physics::offline_simulation();
        self.submitted_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let target = f64::from(ctx.scalar_or_param("target_time", self.submitted_time as f32));
        let dropped_seconds = f64::from(ctx.scalar_or_param("dropped_seconds", 0.0));
        self.submitted_cap = ctx.scalar_or_param("step_cap_hit", 0.0) > 0.0;
        // A graph saved without the domain's interval wire runs on the project's Sim Rate.
        let interval_duration = ctx.scalar_or_param(
            "interval_duration",
            crate::water::physics::simulation_interval() as f32,
        );
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        let substeps = whole(ctx.scalar_or_param("substeps_per_tick", 1.0)).max(1);
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();

        // The grid arrays follow the lattice the region's atoms dispatch over;
        // a lattice the device cannot hold stops the solver before any tick.
        let device = gpu.device;
        let mut refused = None;
        for (buffer, bytes) in [(&mut self.grid_accum, grid_accum_bytes(nodes)), (&mut self.grid, grid_bytes(nodes))] {
            if buffer.as_ref().is_some_and(|b| b.size >= bytes) {
                continue;
            }
            let bytes = bytes.max(32);
            match crate::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer(bytes))
            {
                Ok(created) => *buffer = Some(created),
                Err(error) => {
                    *buffer = None;
                    refused = Some(format!(
                        "Matter: a {}×{}×{} lattice needs {bytes} bytes of grid the device cannot give: {error}. Lower Resolution.",
                        nodes[0], nodes[1], nodes[2]
                    ));
                    break;
                }
            }
        }
        let zero_stats = self
            .zero_stats
            .get_or_insert_with(|| gpu.device.create_buffer(u64::from(STATS_WORDS) * 4));

        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.ticks_done = 0;
            self.completed_time = 0.0;
            self.dropped_time.reset();
            self.completed_cap = false;
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
        self.poll_readbacks(clock.as_ref(), live_mode);
        if live_mode
            && self.faulted
            && let (Some(seed), Some(out)) = (seed, out)
        {
            gpu.native_enc
                .copy_buffer_to_buffer(seed, out, seed.size.min(out.size));
        }
        self.dropped_time.record(
            target,
            self.completed_time,
            dropped_seconds,
            self.completed_cap,
            self.faulted,
        );

        self.substeps = substeps;
        self.step_dt = substep_duration(interval_duration, substeps);
        self.interval = None;
        self.pending = if (self.faulted && !live_mode) || refused.is_some() { 0 } else { ticks.saturating_mul(substeps) };
        self.captures = 0;
        self.captured_ticks = 0;
        let live = self.last_stats.map_or(count, |s| s.live);
        ctx.outputs.set_scalar("live_count", ParamValue::Float(live as f32));
        ctx.outputs.set_scalar("fault", ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }));
        if let Some(error) = refused {
            ctx.error(error);
        } else if self.faulted {
            ctx.error(if live_mode {
                "Matter: non-finite values detected; reseeding while the show continues"
            } else {
                "Matter: a tick produced non-finite values; the liquid is halted until Reset"});
        }
    }

    fn set_substep_interval(&mut self, timing: SubstepInterval) {
        // The domain's metadata cannot restart a halted or empty boundary.
        if self.pending == 0 || timing.iterations == 0 {
            return;
        }
        self.pending = timing.total_iterations;
        self.substeps = timing.iterations;
        self.step_dt = substep_duration(timing.duration().0 as f32, self.substeps);
        self.interval = Some(timing);
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        let (sub, ordinal) = self.interval.map_or_else(
            || (iteration % self.substeps, iteration / self.substeps),
            |timing| (iteration - timing.first_iteration, timing.ordinal),
        );
        let tick = self.ticks_done + u64::from(ordinal);
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
        if !self.capture_iteration() {
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
                    endpoint: 0.0,
                    cap_hit: false,
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
        slot.endpoint = self.submitted_time;
        slot.cap_hit = self.submitted_cap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::effect_node::EffectNode;

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

    #[test]
    fn matter_state_unequal_interval_counts_keep_tick_flags_and_completion() {
        use manifold_core::Seconds;
        let mut state = MatterState::new();
        state.pending = 2 * 2; // Initial scalar count is only the first interval's.
        state.ticks_done = 10;
        let timings = [
            SubstepInterval {
                start: Seconds(0.0), end: Seconds(0.02),
                ordinal: 0, first_iteration: 0, iterations: 2, total_iterations: 7,
            },
            SubstepInterval {
                start: Seconds(0.02), end: Seconds(0.07),
                ordinal: 1, first_iteration: 2, iterations: 5, total_iterations: 7,
            },
        ];
        let mut scalars = [0.0; 6];
        for timing in timings {
            for local in 0..timing.iterations {
                EffectNode::set_substep_interval(&mut state, timing);
                let iteration = timing.first_iteration + local;
                assert!(EffectNode::substep_iteration(&mut state, iteration, &mut scalars));
                assert_eq!(scalars, [
                    substep_duration(timing.duration().0 as f32, timing.iterations),
                    iteration as f32, local as f32,
                    if local == 0 { 1.0 } else { 0.0 },
                    if local + 1 == timing.iterations { 1.0 } else { 0.0 },
                    (10 + timing.ordinal) as f32,
                ]);
                assert_eq!(state.capture_iteration(), iteration == 6);
            }
        }
        assert_eq!(state.pending, 7);
        assert_eq!(state.captured_ticks, 2);
        assert_eq!(state.ticks_done, 12);
        assert!(!EffectNode::substep_iteration(&mut state, 7, &mut scalars));

        state.pending = 0;
        state.faulted = true;
        EffectNode::set_substep_interval(&mut state, timings[0]);
        assert_eq!(state.pending, 0, "interval metadata cannot restart a halted boundary");
        assert!(!EffectNode::substep_iteration(&mut state, 0, &mut scalars));
    }

    #[test]
    fn matter_state_fixed_count_fallback_counts_captured_ticks() {
        let mut state = MatterState::new();
        state.pending = 6;
        state.substeps = 3;
        state.ticks_done = 4;
        for iteration in 0..6 {
            assert_eq!(state.capture_iteration(), iteration == 5);
        }
        assert_eq!(state.captured_ticks, 2);
        assert_eq!(state.ticks_done, 6);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
