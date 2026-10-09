//! `node.liquid_state` — the tick boundary of a particle liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D10): it holds the particles across
//! frames and has the executor run its region once per liquid tick the
//! domain's clock is due. The region body is one whole tick. A non-finite
//! tick (read back through a fenced ring) reports an error and reseeds water
//! without stopping its clock. The last
//! tick's face grid escapes beside the particles into storage this node owns,
//! sized from the lattice wires before the region runs, so it is whole from
//! the first frame and holds while the transport is paused.
//! The optional interior field uses the Ferstl et al. (2016) narrow-band
//! positive sentinel when it is unwired or reset.

use manifold_gpu::{GpuBuffer, GpuDevice};

use super::gpu_flip_step::face_bytes;
use super::particle_identity::{ParticleIdentity, IDENTITY_BYTES};
use super::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
use super::whitewater_step::{DEFAULT_CAPACITY as WHITEWATER_DEFAULT_CAPACITY, MAX_CAPACITY as WHITEWATER_MAX_CAPACITY};
use crate::exec::effect_node::EffectNodeContext;
use crate::particles::{FluidParticle};
use crate::water::fluid_particles::{FaceSample};
use crate::water::liquid::grid::{interior_bytes, InteriorOps};
use crate::water::liquid::lattice::{FlipSolverGrid, LiquidLattice};
use crate::parameters::ParamValue;
use crate::water::physics_metrics::DroppedTimeTracker;
use crate::primitive::Primitive;
use crate::exec::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use crate::water::whitewater::{WHITEWATER_EMPTY, WhitewaterParticle};

const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts {
        capture: "clock_status_in",
        output: "clock_status",
        optional: true,
    },SubstepResultPorts { capture: "stats_in", output: "stats", optional: false },
    SubstepResultPorts { capture: "faces_in", output: "faces", optional: false },
    SubstepResultPorts { capture: "whitewater_pool_in", output: "whitewater_pool", optional: true },
    SubstepResultPorts { capture: "whitewater_state_in", output: "whitewater_state", optional: true },
    SubstepResultPorts { capture: "whitewater_counts_in", output: "whitewater_counts", optional: true },
    SubstepResultPorts { capture: "foam_particles_in", output: "foam_particles", optional: true },
    SubstepResultPorts { capture: "bubble_particles_in", output: "bubble_particles", optional: true },
    SubstepResultPorts { capture: "spray_particles_in", output: "spray_particles", optional: true },
    SubstepResultPorts { capture: "dust_particles_in", output: "dust_particles", optional: true },
    SubstepResultPorts { capture: "interior_in", output: "interior", optional: true },
    SubstepResultPorts { capture: "identity_in", output: "identity", optional: true },
];

/// The region's contract: the tick's index in the epoch and a fenced speed
/// sample, valid only for that exact incoming tick state.
pub const LIQUID_STATE_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &["tick_index", "retired_max_speed"],
    results: RESULTS,
    // The domain owns the clock; offline it may sync between ticks.
    clock: Some("ticks"),
};

/// Stats readbacks in flight: the GPU writes a slot at the end of a frame's
/// region, the CPU reads it once the frame clock says that frame retired.
const READBACK_SLOTS: usize = 3;
const WHITEWATER_STATE_WORDS: u32 = 8;
const WHITEWATER_COUNT_WORDS: u32 = 9;
const WHITEWATER_RESULT_START: usize = 3;
const WHITEWATER_RESULT_COUNT: usize = 7;

pub struct ReadbackSlot {
    buffer: GpuBuffer,
    identity: GpuBuffer,
    clock_status: GpuBuffer,
    stamp: u64,
    epoch: u32,
    pending: bool,
    endpoint: f64,
    /// Exact incoming tick ordinal after the copied final tick completed.
    completed_ticks: u64,
}

/// The last captured tick's stats, identity and clock status, copied when
/// another tick of the frame follows. Read only after a host sync waited for
/// them (`substep_host_synced`).
pub struct SyncSample {
    stats: GpuBuffer,
    identity: GpuBuffer,
    clock_status: GpuBuffer,
}

/// A tick's max marker speed, unless its stats make it unusable for the
/// next tick's CFL decision.
fn usable_speed(stats: &LiquidTickStats) -> Option<f32> {
    (stats.nonfinite == 0
        && stats.narrow_band_shortage == 0
        && stats.max_speed.is_finite()
        && stats.max_speed >= 0.0)
        .then_some(stats.max_speed)
}

fn create_shared_buffer(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    crate::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer_shared(bytes).map_err(|error| error.to_string()))
}

crate::primitive! {
    name: LiquidState,
    type_id: "node.liquid_state",
    purpose: "Hold a particle liquid across frames and run its tick region: seed the particles when the epoch changes, then repeat the region once per due tick with the tick's index. The region's last particles become the state, and its last stats, optional interior distance and its last tick's face grid escape with them; a new epoch's faces are zero. Live GPU FLIP non-finite values report an error and reseed particles without stopping the show or resetting simulation time. Narrow-band capacity shortage halts until Reset; other solver fault handling is unchanged.",
    inputs: {
        seed: Array(FluidParticle) required,
        in: Array(FluidParticle) required,
        stats_in: Array(u32) required,
        identity_in: Array(u32) optional,
        clock_status_in: Array(u32) optional,
        faces_in: Array(FaceSample) required,
        whitewater_pool_in: Array(WhitewaterParticle) optional,
        whitewater_state_in: Array(u32) optional,
        whitewater_counts_in: Array(u32) optional,
        foam_particles_in: Array(FluidParticle) optional,
        bubble_particles_in: Array(FluidParticle) optional,
        spray_particles_in: Array(FluidParticle) optional,
        dust_particles_in: Array(FluidParticle) optional,
        interior_in: Array(f32) optional,
        count: ScalarF32 optional,
        whitewater_capacity: ScalarF32 optional,
        ticks: ScalarF32 optional,
        simulation_time: ScalarF32 optional,
        target_time: ScalarF32 optional,
        dropped_seconds: ScalarF32 optional,
        epoch: ScalarF32 optional,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        identity: Array(u32),
        stats: Array(u32),
        clock_status: Array(u32),
        faces: Array(FaceSample),
        whitewater_pool: Array(WhitewaterParticle),
        whitewater_state: Array(u32),
        whitewater_counts: Array(u32),
        foam_particles: Array(FluidParticle),
        bubble_particles: Array(FluidParticle),
        spray_particles: Array(FluidParticle),
        dust_particles: Array(FluidParticle),
        interior: Array(f32),
        tick_index: ScalarF32,
        retired_max_speed: ScalarF32,
        live_count: ScalarF32,
        fault: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The tick boundary of a particle liquid. seed and count come from the fill; ticks and epoch from the liquid's domain (the region's clock owner). The body is one tick: every step of the solver from out, then node.liquid_stats over the tick's last particles, closing back into in and stats_in; the last step's projected, extended faces close into faces_in, and the domain's nodes_x/y/z size the held faces (required with faces_in). out and stats escape to node.liquid_frame; faces to three node.face_sample_component that feed the frame's face grid.",
    examples: [],
    picker: { label: "Liquid State", category: Atom },
    summary: "Keeps a particle liquid between frames and runs one pass of its simulation per tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid state", "tick loop", "particle state"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        identity: Option<GpuBuffer> = None,
        identity_ops: ParticleIdentity = ParticleIdentity::default(),
        identity_reset: bool = false,
        epoch: Option<u32> = None,
        pending: u32 = 0,
        ticks_done: u64 = 0,
        submitted_time: f64 = 0.0,
        completed_time: f64 = 0.0,
        dropped_time: DroppedTimeTracker = DroppedTimeTracker::default(),
        captures: u32 = 0,
        zero_stats: Option<GpuBuffer> = None,
        readback: Vec<ReadbackSlot> = Vec::new(),
        faulted: bool = false,
        capacity_faulted: bool = false,
        cap_hit: bool = false,
        clock_nonfinite: bool = false,
        last_stats: Option<LiquidTickStats> = None,
        retired_ticks: Option<u64> = None,
        sync_sample: Option<SyncSample> = None,
        // Incoming tick ordinal the sync sample describes, set when copied.
        sync_sample_tick: Option<u64> = None,
        faces: Option<GpuBuffer> = None,
        whitewater_pool: Option<GpuBuffer> = None,
        whitewater_empty: Option<GpuBuffer> = None,
        whitewater_state: Option<GpuBuffer> = None,
        whitewater_counts: Option<GpuBuffer> = None,
        foam_particles: Option<GpuBuffer> = None,
        bubble_particles: Option<GpuBuffer> = None,
        spray_particles: Option<GpuBuffer> = None,
        dust_particles: Option<GpuBuffer> = None,
        whitewater_capacity: u32 = 0,
        interior: Option<GpuBuffer> = None,
        interior_ops: InteriorOps = InteriorOps::default(),
    },
}

impl LiquidState {
    fn ensure_whitewater_buffers(
        &mut self,
        device: &GpuDevice,
        active: [bool; 7],
        capacity: u32,
    ) -> Result<bool, String> {
        let [pool_active, state_active, counts_active, foam_active, bubble_active, spray_active, dust_active] = active;
        let pool_bytes = u64::from(capacity) * std::mem::size_of::<WhitewaterParticle>() as u64;
        let particle_bytes = u64::from(capacity) * std::mem::size_of::<FluidParticle>() as u64;
        let state_bytes = u64::from(WHITEWATER_STATE_WORDS) * 4;
        let count_bytes = u64::from(WHITEWATER_COUNT_WORDS) * 4;
        let mut resized = false;

        let mut ensure = |slot: &mut Option<GpuBuffer>, active: bool, bytes: u64| -> Result<(), String> {
            if !active {
                return Ok(());
            }
            if slot.as_ref().is_some_and(|buffer| buffer.size == bytes) {
                return Ok(());
            }
            *slot = Some(create_shared_buffer(device, bytes.max(4))?);
            resized = true;
            Ok(())
        };
        ensure(&mut self.whitewater_pool, pool_active, pool_bytes)?;
        ensure(&mut self.whitewater_state, state_active, state_bytes)?;
        ensure(&mut self.whitewater_counts, counts_active, count_bytes)?;
        ensure(&mut self.foam_particles, foam_active, particle_bytes)?;
        ensure(&mut self.bubble_particles, bubble_active, particle_bytes)?;
        ensure(&mut self.spray_particles, spray_active, particle_bytes)?;
        ensure(&mut self.dust_particles, dust_active, particle_bytes)?;

        if pool_active && self.whitewater_empty.as_ref().is_none_or(|buffer| buffer.size != pool_bytes) {
            let template = create_shared_buffer(device, pool_bytes.max(4))?;
            let Some(ptr) = template.mapped_ptr() else {
                return Err("whitewater pool's empty template is not CPU-mappable".to_string());
            };
            let empty = WhitewaterParticle { kind: WHITEWATER_EMPTY, ..WhitewaterParticle::default() };
            // SAFETY: the shared buffer is exactly `capacity` records and is
            // private to this node; the template is never written by a GPU pass.
            unsafe {
                std::slice::from_raw_parts_mut(ptr.cast::<WhitewaterParticle>(), capacity as usize).fill(empty);
            }
            self.whitewater_empty = Some(template);
            resized = true;
        }
        self.whitewater_capacity = capacity;
        Ok(resized)
    }

    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(&mut self, clock: Option<&manifold_gpu::FrameClock>, live_recovery: bool) -> bool {
        let Some(epoch) = self.epoch else { return false ;};
        let mut newest: Option<(u64, LiquidTickStats, f64, u64, bool, bool)> = None;
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
            if let Some(ptr) = slot.identity.mapped_ptr() {
                // SAFETY: identity metadata is copied under the same retired fence.
                let identity = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), 4) };
                self.identity_reset |= identity[3] != 0;
            }
            let status = slot.clock_status.mapped_ptr().map(|ptr| {
                // SAFETY: this slot shares the retired fence with its stats.
                unsafe { std::slice::from_raw_parts(ptr.cast::<u32>().cast_const(), 8) }
            });
            let cap_hit = status.is_some_and(|words| words[4] != 0);
            let nonfinite = status.is_some_and(|words| words[5] != 0);
            if let Some(status) = status {
                log::debug!(target: "gpu_flip::schedule",
                    "retired={} endpoint={} steps={} remaining={} maximum={} cap={cap_hit} nonfinite={nonfinite} live={} bad_particles={} kinetic={} pressure={} density={} unconverged={} pockets={} speed_capped={} push_refused={} narrow_shortage={}",
                    slot.completed_ticks, slot.endpoint, status[6], f32::from_bits(status[2]), stats.max_speed,
                    stats.live, stats.nonfinite, stats.kinetic, stats.pressure_iterations, stats.density_iterations,
                    stats.unconverged, stats.unresolved_pockets, stats.speed_capped, stats.push_refused, stats.narrow_band_shortage);
            }
            if newest.is_none_or(|(stamp, _, _, _, _, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, stats, slot.endpoint, slot.completed_ticks, cap_hit, nonfinite));
            }
        }
        if let Some((_, stats, endpoint, completed_ticks, cap_hit, nonfinite)) = newest {
            return self.accept_retired_stats(stats, endpoint, completed_ticks, cap_hit, nonfinite, live_recovery);
        }
        false
    }

    fn accept_retired_stats(
        &mut self,
        stats: LiquidTickStats,
        endpoint: f64,
        completed_ticks: u64,
        cap_hit: bool,
        nonfinite: bool,
        live_recovery: bool,
    ) -> bool {
        self.cap_hit = cap_hit;
        self.clock_nonfinite = nonfinite;
        self.capacity_faulted |= stats.narrow_band_shortage > 0;
        self.faulted = self.capacity_faulted || stats.nonfinite > 0 || (self.faulted && !live_recovery);
        self.completed_time = endpoint;
        self.last_stats = Some(stats);
        self.retired_ticks = (!self.faulted && !nonfinite).then_some(completed_ticks);
        self.faulted && live_recovery && !self.capacity_faulted
    }

    fn reset_epoch(&mut self, epoch: u32) {
        self.completed_time = 0.0;
        self.dropped_time.reset();
        self.epoch = Some(epoch);
        self.ticks_done = 0;
        self.faulted = false;
        self.capacity_faulted = false;
        self.cap_hit = false;
        self.clock_nonfinite = false;
        self.last_stats = None;
        self.retired_ticks = None;
    }

    /// Only a retired sample of this exact incoming tick state is fresh.
    fn retired_speed(&self, iteration: u32) -> f32 {
        if self.faulted || self.capacity_faulted || self.clock_nonfinite || self.identity_reset {
            return -1.0;
        }
        let Some(incoming_tick) = self.ticks_done.checked_add(u64::from(iteration)) else { return -1.0 };
        self.last_stats
            .filter(|_| self.retired_ticks == Some(incoming_tick))
            .as_ref()
            .and_then(usable_speed)
            .unwrap_or(-1.0)
    }

    /// Copy the tick just captured for a host sync before the next tick of
    /// this frame. Without a sync it is never read.
    fn capture_sync_sample(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.sync_sample_tick = None;
        let Some(stats) = ctx.outputs.array("stats") else { return };
        let clock_status = ctx.inputs.array("clock_status_in");
        let gpu = ctx.gpu_encoder();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let sample = self.sync_sample.get_or_insert_with(|| SyncSample {
            stats: gpu.device.create_buffer_shared(stats_bytes),
            identity: gpu.device.create_buffer_shared(IDENTITY_BYTES),
            clock_status: gpu.device.create_buffer_shared(32),
        });
        gpu.native_enc.copy_buffer_to_buffer(stats, &sample.stats, stats_bytes.min(stats.size));
        match self.identity.as_ref() {
            Some(identity) => gpu.native_enc.copy_buffer_to_buffer(identity, &sample.identity, IDENTITY_BYTES),
            None => gpu.native_enc.clear_buffer(&sample.identity),
        }
        match clock_status {
            Some(status) => gpu.native_enc.copy_buffer_to_buffer(status, &sample.clock_status, 32),
            None => gpu.native_enc.clear_buffer(&sample.clock_status),
        }
        self.sync_sample_tick = Some(self.ticks_done + u64::from(self.captures));
    }

    /// The sync sample's speed when it describes exactly this iteration's
    /// incoming tick and neither it nor the liquid's state rules it out
    /// (the same rules as `retired_speed`).
    fn synced_speed(&self, iteration: u32, sample_tick: u64, stats: &LiquidTickStats, nonfinite_clock: bool, identity_reset: bool) -> Option<f32> {
        if self.faulted || self.capacity_faulted || self.clock_nonfinite || self.identity_reset || nonfinite_clock || identity_reset {
            return None;
        }
        if self.ticks_done.checked_add(u64::from(iteration)) != Some(sample_tick) {
            return None;
        }
        usable_speed(stats)
    }
}

impl Primitive for LiquidState {
    fn take_substep_restart_request(&mut self) -> bool {
        std::mem::take(&mut self.identity_reset)
    }

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.interior_ops.prepare_clear(device);
        self.identity_ops.prepare(device);
    }

    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &[
            "in",
            "stats_in",
            "identity_in",
            "clock_status_in","faces_in",
            "whitewater_pool_in",
            "whitewater_state_in",
            "whitewater_counts_in",
            "foam_particles_in",
            "bubble_particles_in",
            "spray_particles_in",
            "dust_particles_in",
            "interior_in",
        ]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &[
            "out",
            "identity",
            "stats",
            "clock_status",
            "whitewater_pool",
            "whitewater_state",
            "whitewater_counts",
            "foam_particles",
            "bubble_particles",
            "spray_particles",
            "dust_particles",
        ]
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "identity" | "faces"
                | "whitewater_pool"
                | "whitewater_state"
                | "whitewater_counts"
                | "foam_particles"
                | "bubble_particles"
                | "spray_particles"
                | "dust_particles"
                | "interior"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "identity" => self.identity.as_ref(),
            "faces" => self.faces.as_ref(),
            "whitewater_pool" => self.whitewater_pool.as_ref(),
            "whitewater_state" => self.whitewater_state.as_ref(),
            "whitewater_counts" => self.whitewater_counts.as_ref(),
            "foam_particles" => self.foam_particles.as_ref(),
            "bubble_particles" => self.bubble_particles.as_ref(),
            "spray_particles" => self.spray_particles.as_ref(),
            "dust_particles" => self.dust_particles.as_ref(),
            "interior" => self.interior.as_ref(),
            _ => None,
        }
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(LIQUID_STATE_PORTS)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities.iter().find(|(p, _)| *p == "seed").map(|&(_, n)| n),
            "stats" => Some(LIQUID_STATS_WORDS),
            "identity" => Some(4),
            "clock_status" => Some(8),
            // Provided storage: a one-record hint, sized at run time from the
            // body's faces, which the plan allocates after this node.
            "faces"
            | "whitewater_pool"
            | "whitewater_state"
            | "whitewater_counts"
            | "foam_particles"
            | "bubble_particles"
            | "spray_particles"
                | "dust_particles"
            | "interior" => Some(1),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let count = whole(ctx.scalar_or_param("count", 0.0));
        let capacity = ctx.scalar_or_param("whitewater_capacity", WHITEWATER_DEFAULT_CAPACITY as f32).round();
        if ctx.inputs.slot("whitewater_pool_in").is_some() && !(1.0..=WHITEWATER_MAX_CAPACITY as f32).contains(&capacity) {
            self.pending = 0;
            ctx.error(format!("Liquid State: whitewater capacity {capacity} is outside 1 to {WHITEWATER_MAX_CAPACITY}"));
            return;
        }
        let whitewater_capacity = capacity as u32;
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        self.submitted_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let target_time = ctx.inputs.scalar("target_time").and_then(|value| match value {
            ParamValue::Float(time) => Some(f64::from(time)),
            _ => None,
        });
        let dropped_seconds = f64::from(ctx.scalar_or_param("dropped_seconds", 0.0));
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let mut refused = None;
        // The face grid's bytes, from the lattice this frame's ticks run on:
        // faces_in still holds the last frame's (or no) body here.
        let face_grid = if ctx.inputs.slot("faces_in").is_none() {
            None
        } else if ["nodes_x", "nodes_y", "nodes_z"].iter().any(|port| ctx.inputs.slot(port).is_none()) {
            refused = Some("Liquid State: faces_in needs the lattice on nodes_x, nodes_y and nodes_z".to_string());
            None
        } else {
            LiquidLattice::from_wires(ctx, "Liquid State").map(|lattice| face_bytes(FlipSolverGrid::from_lattice(lattice).cells()))
        }
        .filter(|_| ctx.outputs.array("faces").is_some());
        let whitewater_active: [bool; WHITEWATER_RESULT_COUNT] = std::array::from_fn(|i| {
            ctx.inputs
                .slot(RESULTS[WHITEWATER_RESULT_START + i].capture)
                .is_some()
        });
        let interior_grid = if ctx.inputs.slot("interior_in").is_none() {
            None
        } else if ["nodes_x", "nodes_y", "nodes_z"].iter().any(|port| ctx.inputs.slot(port).is_none()) {
            refused = Some("Liquid State: interior_in needs the lattice on nodes_x, nodes_y and nodes_z".to_string());
            None
        } else {
            LiquidLattice::from_wires(ctx, "Liquid State").map(|lattice| interior_bytes(FlipSolverGrid::from_lattice(lattice).cells()))
        };
        if interior_grid == Some(0) {
            refused = Some("Liquid State: interior distance has zero cells".to_string());
        }
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let live_recovery = target_time.is_some() && !crate::water::physics::offline_simulation();
        let recover = self.poll_readbacks(clock.as_ref(), live_recovery);
        if self.identity_reset && self.epoch == Some(epoch) {
            // The executor restarts the domain clock (and coupled rigid state).
            // Until its fresh epoch arrives, the allocator refuses all births
            // and the publisher keeps the last accepted frame.
            self.pending = 0;
            return;
        }
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let zero_stats = self.zero_stats.get_or_insert_with(|| gpu.device.create_buffer(stats_bytes)).clone();
        let old_whitewater_capacity = self.whitewater_capacity;
        let whitewater_resized = match self.ensure_whitewater_buffers(gpu.device, whitewater_active, whitewater_capacity) {
            Ok(resized) => resized,
            Err(error) => {
                refused = Some(format!("Liquid State: whitewater buffers could not be allocated: {error}"));
                false
            }
        };
        let whitewater_reset = whitewater_resized || old_whitewater_capacity != whitewater_capacity;

        // The faces are the lattice's face grid, held across frames; unwired
        // or refused, none.
        let mut fresh_faces = false;
        match face_grid {
            None => self.faces = None,
            Some(bytes) if self.faces.as_ref().is_none_or(|f| f.size != bytes) => {
                let device = gpu.device;
                self.faces = crate::load::expand::admit_candidate_bytes(
                    device.modifier_memory_snapshot(),
                    bytes,
                )
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer_shared(bytes))
                .map_err(|error| {
                    refused = Some(format!(
                        "Liquid State: the face grid needs {bytes} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                })
                .ok();
                fresh_faces = self.faces.is_some();
            }
            Some(_) => {}
        }

        let mut fresh_interior = false;
        match interior_grid.filter(|&bytes| bytes != 0) {
            None => self.interior = None,
            Some(bytes) if self.interior.as_ref().is_none_or(|field| field.size != bytes) => {
                let device = gpu.device;
                self.interior = crate::load::expand::admit_candidate_bytes(
                    device.modifier_memory_snapshot(),
                    bytes,
                )
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer_shared(bytes.max(4)))
                .map_err(|error| {
                    refused = Some(format!(
                        "Liquid State: the interior distance needs {bytes} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                })
                .ok();
                fresh_interior = self.interior.is_some();
            }
            Some(_) => {}
        }

        if self.epoch != Some(epoch) || fresh_faces || self.identity_reset {
            // A new epoch's faces are zero until its first tick.
            if let Some(faces) = &self.faces {
                gpu.native_enc.clear_buffer(faces);
            }
        }
        let epoch_reset = self.epoch != Some(epoch) || self.identity_reset;
        // Recovery increments on the GPU, after all submitted births. Retired
        // CPU metadata may be older than the allocator currently on the queue.
        let seed_identity_epoch = if self.epoch != Some(epoch) { 0 } else { u32::MAX };
        if epoch_reset {
            self.reset_epoch(epoch);
        }
        let identity = self.identity.get_or_insert_with(|| gpu.device.create_buffer_shared(IDENTITY_BYTES));
        if (epoch_reset || fresh_interior)
            && let Some(interior) = self.interior.as_ref()
            && let Err(error) = self.interior_ops.clear(gpu, interior)
        {
            refused = Some(format!("Liquid State: {error}"));
        }
        if epoch_reset {
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * std::mem::size_of::<FluidParticle>() as u64).min(seed.size).min(out.size);
                if bytes > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                }
            }
            if let Some(out) = out {
                self.identity_ops.seed(gpu.native_enc, out, identity, count.min((out.size / 32) as u32), seed_identity_epoch);
            }
            if let Some(stats) = stats {
                gpu.native_enc.clear_buffer(&zero_stats);
                gpu.native_enc.copy_buffer_to_buffer(&zero_stats, stats, stats_bytes.min(stats.size));
            }
        }
        if epoch_reset || whitewater_reset {
            if let (Some(template), Some(pool)) = (&self.whitewater_empty, &self.whitewater_pool) {
                gpu.native_enc.copy_buffer_to_buffer(template, pool, template.size.min(pool.size));
            }
            for buffer in [
                self.whitewater_state.as_ref(),
                self.whitewater_counts.as_ref(),
                self.foam_particles.as_ref(),
                self.bubble_particles.as_ref(),
                self.spray_particles.as_ref(),
                self.dust_particles.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                gpu.native_enc.clear_buffer(buffer);
            }
        }
        if recover {
            self.retired_ticks = None;
            // Replace poisoned state in encoder order without reanchoring time.
            // A numerical fault must not latch a show stop until manual Reset.
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * size_of::<FluidParticle>() as u64).min(seed.size).min(out.size);
                gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                self.identity_ops.seed(gpu.native_enc, out, identity, count.min((out.size / 32) as u32), seed_identity_epoch);
            }
        }
        self.identity_reset = false;
        if let Some(target) = target_time {
            self.dropped_time.record(
                target,
                self.completed_time,
                dropped_seconds,
                self.cap_hit,
                self.faulted || self.clock_nonfinite,
            );
        }

        self.pending = if refused.is_some() || self.capacity_faulted || (self.faulted && !live_recovery) { 0 } else { ticks };
        self.captures = 0;
        self.sync_sample_tick = None;
        let live =self.last_stats.map_or(count, |s| s.live);
        ctx.outputs.set_scalar("live_count", ParamValue::Float(live as f32));
        ctx.outputs.set_scalar("fault", ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }));
        if self.faulted {
            if self.last_stats.is_some_and(|s| s.nonfinite > 0) {
            ctx.error(if live_recovery {
                "Liquid State: non-finite values detected; reseeding water while the show continues"
            } else {
                "Liquid State: a tick produced non-finite values; the liquid is halted until Reset"
            });
            }
            if self.last_stats.is_some_and(|s| s.narrow_band_shortage > 0) {
                ctx.error("Liquid State: narrow-band particle capacity was insufficient for reseeding; the liquid is halted until Reset");
            }
        }
        if let Some(error) = refused {
            ctx.error(error);
        }
        // A speed-capped move, a refused push and a capped solve are the
        // solver's limits, not faults: the water keeps moving, and the stats
        // words (speed_capped, push_refused, unconverged) report them.
        if let Some(stats) = self.last_stats.filter(|s| s.unresolved_pockets > 0) {
            ctx.error(format!(
                "Liquid State: {} steps last tick could not tell which water a solid seals off from air: the spread reached its cap, one round per cell of the lattice's longest side, unfinished",
                stats.unresolved_pockets
            ));
        }
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        scalars[0] = (self.ticks_done + u64::from(iteration)) as f32;
        scalars[1] = self.retired_speed(iteration);
        true
    }

    /// A coupled liquid's later tick: the host sync waited for the tick
    /// before it, so that tick's copy is a fresh, fenced speed sample.
    fn substep_host_synced(&mut self, iteration: u32, scalars: &mut [f32]) {
        let Some(tick) = self.sync_sample_tick.take() else { return };
        let Some(sample) = self.sync_sample.as_ref() else { return };
        let (Some(stats), Some(identity), Some(status)) =
            (sample.stats.mapped_ptr(), sample.identity.mapped_ptr(), sample.clock_status.mapped_ptr())
        else {
            return;
        };
        // SAFETY: shared buffers of these sizes, and the executor committed
        // and waited for every command before calling this.
        let (stats, identity_reset, nonfinite_clock) = unsafe {
            (
                LiquidTickStats::from_words(std::slice::from_raw_parts(stats.cast::<u32>().cast_const(), LIQUID_STATS_WORDS as usize)),
                *identity.cast::<u32>().cast_const().add(3) != 0,
                *status.cast::<u32>().cast_const().add(5) != 0,
            )
        };
        if let Some(speed) = self.synced_speed(iteration, tick, &stats, nonfinite_clock, identity_reset) {
            scalars[1] = speed;
        }
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A body that writes fresh storage is accepted by copy.
        for (candidate, state) in [
            ("in", "out"),
            ("stats_in", "stats"),
            ("identity_in", "identity"),
            ("clock_status_in", "clock_status"),
            ("whitewater_pool_in", "whitewater_pool"),
            ("whitewater_state_in", "whitewater_state"),
            ("whitewater_counts_in", "whitewater_counts"),
            ("foam_particles_in", "foam_particles"),
            ("bubble_particles_in", "bubble_particles"),
            ("spray_particles_in", "spray_particles"),
            ("dust_particles_in", "dust_particles"),
        ] {
            if let (Some(candidate), Some(state)) = (ctx.inputs.array(candidate), ctx.outputs.array(state))
                && !candidate.ptr_eq(state)
            {
                let size = candidate.size.min(state.size);
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, state, size);
            }
        }
        self.captures += 1;
        if self.captures < self.pending {
            self.capture_sync_sample(ctx);
        }
        if self.captures != self.pending {
            return;
        }
        // Only the frame's last tick reaches the faces.
        if let (Some(candidate), Some(faces)) = (ctx.inputs.array("faces_in"), self.faces.as_ref()) {
            if candidate.size == faces.size {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, faces, faces.size);
            } else {
                ctx.error(format!(
                    "Liquid State: the tick's faces hold {} bytes; the lattice's face grid is {}",
                    candidate.size, faces.size
                ));
            }
        }
        if let (Some(candidate), Some(interior)) = (ctx.inputs.array("interior_in"), self.interior.as_ref()) {
            if candidate.size == interior.size {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, interior, interior.size);
            } else {
                ctx.error(format!(
                    "Liquid State: the tick's interior distance holds {} bytes; the lattice's cell-centred field is {}",
                    candidate.size, interior.size
                ));
            }
        }
        self.ticks_done += u64::from(self.pending);
        let Some(stats) = ctx.outputs.array("stats") else { return };
        let Some(epoch) = self.epoch else { return };
        let clock_status = ctx.inputs.array("clock_status_in");
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let complete = |slot: &ReadbackSlot| !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp));
        let index = match self.readback.iter().position(complete) {
            Some(index) => index,
            None if self.readback.len() < READBACK_SLOTS => {
                self.readback.push(ReadbackSlot {
                    buffer: gpu.device.create_buffer_shared(stats_bytes),
                    identity: gpu.device.create_buffer_shared(IDENTITY_BYTES),
                    clock_status: gpu.device.create_buffer_shared(32),
                    stamp: 0,
                    epoch,
                    pending: false,
                    endpoint: 0.0,
                    completed_ticks: 0,
                });
                self.readback.len() - 1
            }
            // Every slot is still in flight: skip this frame's readback; live
            // never waits on the GPU.
            None => return,
        };
        let slot = &mut self.readback[index];
        gpu.native_enc.copy_buffer_to_buffer(stats, &slot.buffer, stats_bytes.min(stats.size));
        if let Some(identity) = self.identity.as_ref() {
            gpu.native_enc.copy_buffer_to_buffer(identity, &slot.identity, IDENTITY_BYTES);
        }
        if let Some(status) = clock_status {
            gpu.native_enc
                .copy_buffer_to_buffer(status, &slot.clock_status, 32);
        } else {
            gpu.native_enc.clear_buffer(&slot.clock_status);
        }
        slot.stamp = clock.as_ref().map_or(0, |c| c.stamp());
        slot.epoch = epoch;
        slot.pending = true;
        slot.endpoint = self.submitted_time;
        slot.completed_ticks = self.ticks_done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::effect_node::EffectNode;

    #[test]
    fn liquid_state_serves_the_tick_index_per_iteration() {
        let mut state = LiquidState::new();
        state.pending = 3;
        state.ticks_done = 10;
        let mut scalars = [0.0f32; 2];
        let mut seen = Vec::new();
        for i in 0.. {
            if !EffectNode::substep_iteration(&mut state, i, &mut scalars) {
                break;
            }
            seen.push(scalars[0]);
            assert_eq!(scalars[1], -1.0);
        }
        assert_eq!(seen, [10.0, 11.0, 12.0]);
        assert_eq!(LIQUID_STATE_PORTS.iteration_scalars, ["tick_index", "retired_max_speed"]);
    }

    fn retired_state(speed: f32) -> LiquidState {
        let mut state = LiquidState::new();
        state.epoch = Some(1);
        state.pending = 3;
        state.ticks_done = 10;
        state.accept_retired_stats(LiquidTickStats { max_speed: speed, ..LiquidTickStats::default() }, 1.0, 10, false, false, true);
        state
    }

    #[test]
    fn liquid_state_retired_speed_is_fresh_only_for_the_incoming_tick() {
        let mut state = retired_state(3.5);
        let mut scalars = [0.0; 2];
        for (iteration, expected) in [(0, 3.5), (1, -1.0), (2, -1.0)] {
            assert!(EffectNode::substep_iteration(&mut state, iteration, &mut scalars));
            assert_eq!(scalars[1], expected);
        }
        // Skipping a full readback ring leaves the previous sample untouched,
        // while successfully captured ticks still advance the incoming state.
        state.ticks_done += 3;
        assert_eq!(state.retired_speed(0), -1.0);
        state.retired_ticks = None;
        assert_eq!(state.retired_speed(0), -1.0);
    }

    #[test]
    fn liquid_state_sync_sample_is_fresh_only_for_its_exact_tick() {
        let mut state = LiquidState::new();
        state.ticks_done = 10;
        let stats = LiquidTickStats { max_speed: 1.5, ..LiquidTickStats::default() };
        assert_eq!(state.synced_speed(1, 11, &stats, false, false), Some(1.5));
        assert_eq!(state.synced_speed(1, 10, &stats, false, false), None, "a tick behind");
        assert_eq!(state.synced_speed(2, 11, &stats, false, false), None, "a tick ahead");
        assert_eq!(state.synced_speed(1, 11, &stats, true, false), None, "nonfinite clock input");
        assert_eq!(state.synced_speed(1, 11, &stats, false, true), None, "identity reset in the sample");
        for bad in [
            LiquidTickStats { nonfinite: 1, ..stats },
            LiquidTickStats { narrow_band_shortage: 1, ..stats },
            LiquidTickStats { max_speed: f32::NAN, ..stats },
            LiquidTickStats { max_speed: -1.0, ..stats },
        ] {
            assert_eq!(state.synced_speed(1, 11, &bad, false, false), None, "{bad:?}");
        }
        state.faulted = true;
        assert_eq!(state.synced_speed(1, 11, &stats, false, false), None, "a faulted liquid");
    }

    #[test]
    fn liquid_state_retired_speed_freshness_uses_exact_integer_ticks() {
        let mut state = retired_state(1.0);
        state.ticks_done = 1 << 24;
        state.retired_ticks = Some(state.ticks_done);
        assert_eq!(state.ticks_done as f32, (state.ticks_done + 1) as f32);
        assert_eq!(state.retired_speed(0), 1.0);
        assert_eq!(state.retired_speed(1), -1.0);
        state.ticks_done = u64::MAX;
        state.retired_ticks = Some(u64::MAX);
        assert_eq!(state.retired_speed(1), -1.0);
    }

    #[test]
    fn liquid_state_retired_speed_rejects_invalid_samples_and_faults() {
        assert_eq!(retired_state(0.0).retired_speed(0), 0.0);
        for speed in [f32::NAN, f32::INFINITY, -0.5] {
            assert_eq!(retired_state(speed).retired_speed(0), -1.0);
        }
        for fault in 0..6 {
            let mut state = retired_state(2.0);
            match fault {
                0 => state.faulted = true,
                1 => state.capacity_faulted = true,
                2 => state.clock_nonfinite = true,
                3 => state.identity_reset = true,
                4 => state.last_stats.as_mut().unwrap().nonfinite = 1,
                5 => state.last_stats.as_mut().unwrap().narrow_band_shortage = 1,
                _ => unreachable!(),
            }
            assert_eq!(state.retired_speed(0), -1.0, "fault {fault}");
        }
        let mut state = retired_state(2.0);
        state.cap_hit = true;
        let stats = state.last_stats.as_mut().unwrap();
        stats.speed_capped = 1;
        stats.unconverged = 1;
        assert_eq!(state.retired_speed(0), 2.0, "ordinary caps do not invalidate the final speed");
    }

    #[test]
    fn liquid_state_retired_speed_epoch_reset_invalidates_the_sample() {
        let mut state = retired_state(2.0);
        state.reset_epoch(2);
        assert_eq!(state.epoch, Some(2));
        assert_eq!(state.retired_ticks, None);
        assert_eq!(state.last_stats, None);
        assert_eq!(state.retired_speed(0), -1.0);
        // An old ordinal must not become fresh when a new epoch reaches it.
        state.ticks_done = 10;
        assert_eq!(state.retired_speed(0), -1.0);
    }

    #[test]
    fn liquid_state_retired_speed_recovery_invalidates_the_captured_tick() {
        let mut state = retired_state(2.0);
        let poisoned = LiquidTickStats { max_speed: 2.0, nonfinite: 1, ..LiquidTickStats::default() };
        assert!(state.accept_retired_stats(poisoned, 1.0, 10, false, false, true));
        assert_eq!(state.retired_ticks, None);
        assert_eq!(state.retired_speed(0), -1.0);
        // Reseeding does not make the old diagnostic describe the new state.
        state.faulted = false;
        state.last_stats.as_mut().unwrap().nonfinite = 0;
        assert_eq!(state.retired_speed(0), -1.0);
        let healthy = LiquidTickStats { max_speed: 0.0, ..LiquidTickStats::default() };
        assert!(!state.accept_retired_stats(healthy, 2.0, 10, false, true, true));
        assert_eq!(state.retired_ticks, None, "clock faults also invalidate the captured tick");
        assert_eq!(state.retired_speed(0), -1.0);
    }

    #[test]
    fn liquid_state_whitewater_results_exclude_clock_and_include_dust() {
        let captures: Vec<_> = RESULTS[WHITEWATER_RESULT_START..WHITEWATER_RESULT_START + WHITEWATER_RESULT_COUNT]
            .iter().map(|r| r.capture).collect();
        assert_eq!(captures, ["whitewater_pool_in", "whitewater_state_in", "whitewater_counts_in", "foam_particles_in", "bubble_particles_in", "spray_particles_in", "dust_particles_in"]);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
