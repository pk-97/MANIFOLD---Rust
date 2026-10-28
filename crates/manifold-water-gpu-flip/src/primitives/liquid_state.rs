//! `node.liquid_state` — the tick boundary of a particle liquid
//!It holds the particles across
//! frames and has the executor run its region once per liquid tick the
//! domain's clock is due. The region body is one whole tick. A non-finite
//! tick (read back through a fenced ring) reports an error and reseeds water
//! without stopping its clock.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBuffer, GpuDevice};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::exec::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::primitives::liquid_stats::{LIQUID_STATS_WORDS, LiquidTickStats};
use manifold_water_liquid::primitives::particle_identity::{IDENTITY_BYTES, ParticleIdentity};
use manifold_water_liquid::whitewater::{
    DEFAULT_CAPACITY as WHITEWATER_DEFAULT_CAPACITY, MAX_CAPACITY as WHITEWATER_MAX_CAPACITY,
};
use manifold_water_liquid::whitewater::{WHITEWATER_EMPTY, WhitewaterParticle};
use manifold_water_rigid::physics_metrics::DroppedTimeTracker;

use record_copy::RecordCopy;

const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts {
        capture: "clock_status_in",
        output: "clock_status",
        optional: true,
    },
    SubstepResultPorts {
        capture: "stats_in",
        output: "stats",
        optional: false,
    },
    SubstepResultPorts {
        capture: "whitewater_pool_in",
        output: "whitewater_pool",
        optional: true,
    },
    SubstepResultPorts {
        capture: "whitewater_state_in",
        output: "whitewater_state",
        optional: true,
    },
    SubstepResultPorts {
        capture: "whitewater_counts_in",
        output: "whitewater_counts",
        optional: true,
    },
    SubstepResultPorts {
        capture: "foam_particles_in",
        output: "foam_particles",
        optional: true,
    },
    SubstepResultPorts {
        capture: "bubble_particles_in",
        output: "bubble_particles",
        optional: true,
    },
    SubstepResultPorts {
        capture: "spray_particles_in",
        output: "spray_particles",
        optional: true,
    },
    SubstepResultPorts {
        capture: "dust_particles_in",
        output: "dust_particles",
        optional: true,
    },
    SubstepResultPorts {
        capture: "identity_in",
        output: "identity",
        optional: true,
    },
    SubstepResultPorts {
        capture: "interior_in",
        output: "interior",
        optional: true,
    },
    SubstepResultPorts {
        capture: "interior_tiles_in",
        output: "interior_tiles",
        optional: true,
    },
];

/// The region's contract: the tick's index in the epoch.
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
const WHITEWATER_STATE_WORDS: u32 = 8;
const WHITEWATER_COUNT_WORDS: u32 = 9;
const WHITEWATER_RESULT_START: usize = 2;
const WHITEWATER_RESULT_COUNT: usize = 7;

pub struct ReadbackSlot {
    buffer: GpuBuffer,
    identity: GpuBuffer,
    clock_status: GpuBuffer,
    stamp: u64,
    epoch: u32,
    pending: bool,
    endpoint: f64,
}

/// Whether `node` is a liquid state whose last read tick had an inflow or a
/// sheet birth wanting a slot the pool did not have: the pool is full and
/// both are held back alike. The water runtime puts it on the domain's setup
/// report.
pub fn births_held_back(node: &dyn manifold_node_engine::exec::effect_node::EffectNode) -> bool {
    node.as_any()
        .downcast_ref::<LiquidState>()
        .and_then(|state| state.last_stats)
        .is_some_and(|stats| stats.inflow_refused > 0 || stats.sheet_refused > 0)
}

/// How many particles `node`'s last read tick removed because their tile got
/// no pool slot, 0 when `node` is not a liquid state or has read no tick. The
/// water runtime puts it on the domain's setup report.
pub fn particles_removed_for_tiles(
    node: &dyn manifold_node_engine::exec::effect_node::EffectNode,
) -> u32 {
    node.as_any()
        .downcast_ref::<LiquidState>()
        .and_then(|state| state.last_stats)
        .map_or(0, |stats| stats.tile_removed)
}

/// Leaves `stats` on a liquid state as its last retired tick, for tests of
/// what reads them without a GPU step.
#[cfg(any(test, feature = "testkit"))]
pub fn retire_stats_for_test(
    node: &mut dyn manifold_node_engine::exec::effect_node::EffectNode,
    stats: LiquidTickStats,
) {
    node.as_any_mut()
        .downcast_mut::<LiquidState>()
        .expect("a liquid state")
        .last_stats = Some(stats);
}

fn create_shared_buffer(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    manifold_node_engine::load::expand::admit_candidate_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| error.to_string())
    .and_then(|()| {
        device
            .try_create_buffer(bytes, BufferUse::Gpu)
            .map_err(|error| error.to_string())
    })
}

manifold_node_engine::primitive! {
    name: LiquidState,
    type_id: "node.liquid_state",
    purpose: "Hold a particle liquid across frames and run its tick region: seed the particles when the epoch changes, then repeat the region once per due tick with the tick's index. The region's last particles become the state, and its last stats escape with them. Live GPU FLIP non-finite values report an error and reseed particles without stopping the show or resetting simulation time.",
    inputs: {
        seed: Array(FluidParticle) required,
        in: Array(FluidParticle) required,
        stats_in: Array(u32) required,
        identity_in: Array(u32) optional,
        clock_status_in: Array(u32) optional,
        whitewater_pool_in: Array(WhitewaterParticle) optional,
        whitewater_state_in: Array(u32) optional,
        whitewater_counts_in: Array(u32) optional,
        foam_particles_in: Array(FluidParticle) optional,
        bubble_particles_in: Array(FluidParticle) optional,
        spray_particles_in: Array(FluidParticle) optional,
        dust_particles_in: Array(FluidParticle) optional,
        interior_in: Array(f32) optional,
        interior_tiles_in: Array(u32) optional,
        count: ScalarF32 optional,
        whitewater_capacity: ScalarF32 optional,
        ticks: ScalarF32 optional,
        simulation_time: ScalarF32 optional,
        target_time: ScalarF32 optional,
        dropped_seconds: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        identity: Array(u32),
        stats: Array(u32),
        clock_status: Array(u32),
        whitewater_pool: Array(WhitewaterParticle),
        whitewater_state: Array(u32),
        whitewater_counts: Array(u32),
        foam_particles: Array(FluidParticle),
        bubble_particles: Array(FluidParticle),
        spray_particles: Array(FluidParticle),
        dust_particles: Array(FluidParticle),
        // GPU FLIP Narrow Band's grid interior and the step's table it is
        // stored on, as the tick's last step left them.
        interior: Array(f32),
        interior_tiles: Array(u32),
        tick_index: ScalarF32,
        fault: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The tick boundary of a particle liquid. seed and count come from the fill; ticks and epoch from the liquid's domain (the region's clock owner). The body is one tick: every step of the solver from out, then node.liquid_stats over the tick's last particles, closing back into in and stats_in. out and stats escape to node.liquid_frame. With GPU FLIP Narrow Band on, wire the step's interior and tiles into interior_in and interior_tiles_in: the display reads them from interior and interior_tiles, each the size the step's pool gives it.",
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
        record_copy: Option<RecordCopy> = None,
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
        cap_hit: bool = false,
        clock_nonfinite: bool = false,
        last_stats: Option<LiquidTickStats> = None,
        whitewater_pool: Option<GpuBuffer> = None,
        whitewater_empty: Option<GpuBuffer> = None,
        whitewater_state: Option<GpuBuffer> = None,
        whitewater_counts: Option<GpuBuffer> = None,
        foam_particles: Option<GpuBuffer> = None,
        bubble_particles: Option<GpuBuffer> = None,
        spray_particles: Option<GpuBuffer> = None,
        dust_particles: Option<GpuBuffer> = None,
        interior: Option<GpuBuffer> = None,
        interior_tiles: Option<GpuBuffer> = None,
        whitewater_capacity: u32 = 0,
    },
}

impl LiquidState {
    fn ensure_whitewater_buffers(
        &mut self,
        device: &GpuDevice,
        active: [bool; 7],
        capacity: u32,
    ) -> Result<bool, String> {
        let [
            pool_active,
            state_active,
            counts_active,
            foam_active,
            bubble_active,
            spray_active,
            dust_active,
        ] = active;
        let pool_bytes = u64::from(capacity) * std::mem::size_of::<WhitewaterParticle>() as u64;
        let particle_bytes = u64::from(capacity) * std::mem::size_of::<FluidParticle>() as u64;
        let state_bytes = u64::from(WHITEWATER_STATE_WORDS) * 4;
        let count_bytes = u64::from(WHITEWATER_COUNT_WORDS) * 4;
        let mut resized = false;

        let mut ensure =
            |slot: &mut Option<GpuBuffer>, active: bool, bytes: u64| -> Result<(), String> {
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

        if pool_active
            && self
                .whitewater_empty
                .as_ref()
                .is_none_or(|buffer| buffer.size != pool_bytes)
        {
            let template = create_shared_buffer(device, pool_bytes.max(4))?;
            let ptr = template
                .mapped_ptr()
                .expect("whitewater pool template must be CPU-visible");
            let empty = WhitewaterParticle {
                kind: WHITEWATER_EMPTY,
                ..WhitewaterParticle::default()
            };
            // SAFETY: the shared buffer is exactly `capacity` records and is
            // private to this node; the template is never written by a GPU pass.
            unsafe {
                std::slice::from_raw_parts_mut(ptr.cast::<WhitewaterParticle>(), capacity as usize)
                    .fill(empty);
            }
            self.whitewater_empty = Some(template);
            resized = true;
        }
        self.whitewater_capacity = capacity;
        Ok(resized)
    }

    /// Read every retired readback of the current epoch, newest last.
    fn poll_readbacks(
        &mut self,
        clock: Option<&manifold_gpu::FrameClock>,
        live_recovery: bool,
    ) -> bool {
        let Some(epoch) = self.epoch else {
            return false;
        };
        let mut newest: Option<(u64, LiquidTickStats, f64, bool, bool)> = None;
        for slot in self.readback.iter_mut().filter(|s| s.pending) {
            if !clock.is_none_or(|c| c.is_complete(slot.stamp)) {
                continue;
            }
            slot.pending = false;
            if slot.epoch != epoch {
                continue;
            }
            let ptr = slot
                .buffer
                .mapped_ptr()
                .expect("liquid stats slot must be CPU-visible");
            // SAFETY: the frame that wrote this shared slot has retired.
            let words = unsafe {
                std::slice::from_raw_parts(
                    ptr.cast::<u32>().cast_const(),
                    LIQUID_STATS_WORDS as usize,
                )
            };
            let stats = LiquidTickStats::from_words(words);
            let identity_ptr = slot
                .identity
                .mapped_ptr()
                .expect("liquid identity slot must be CPU-visible");
            // SAFETY: identity metadata is copied under the same retired fence.
            let identity = unsafe { std::slice::from_raw_parts(identity_ptr.cast::<u32>(), 4) };
            self.identity_reset |= identity[3] != 0;
            let status_ptr = slot
                .clock_status
                .mapped_ptr()
                .expect("liquid clock status slot must be CPU-visible");
            // SAFETY: this slot shares the retired fence with its stats.
            let status =
                unsafe { std::slice::from_raw_parts(status_ptr.cast::<u32>().cast_const(), 8) };
            let cap_hit = status[4] != 0;
            let nonfinite = status[5] != 0;
            log::debug!(target: "gpu_flip::schedule",
                "endpoint={} steps={} remaining={} maximum={} cap={cap_hit} nonfinite={nonfinite} live={} bad_particles={} kinetic={} pressure={} density={} unconverged={} pockets={} speed_capped={} push_refused={}",
                slot.endpoint, status[6], f32::from_bits(status[2]), stats.max_speed,
                stats.live, stats.nonfinite, stats.kinetic, stats.pressure_iterations, stats.density_iterations,
                stats.unconverged, stats.unresolved_pockets, stats.speed_capped, stats.push_refused);
            if newest.is_none_or(|(stamp, _, _, _, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, stats, slot.endpoint, cap_hit, nonfinite));
            }
        }
        if let Some((_, stats, endpoint, cap_hit, nonfinite)) = newest {
            return self.accept_retired_stats(stats, endpoint, cap_hit, nonfinite, live_recovery);
        }
        false
    }

    fn accept_retired_stats(
        &mut self,
        stats: LiquidTickStats,
        endpoint: f64,
        cap_hit: bool,
        nonfinite: bool,
        live_recovery: bool,
    ) -> bool {
        self.cap_hit = cap_hit;
        self.clock_nonfinite = nonfinite;
        self.faulted = stats.nonfinite > 0 || (self.faulted && !live_recovery);
        self.completed_time = endpoint;
        self.last_stats = Some(stats);
        self.faulted && live_recovery
    }

    fn reset_epoch(&mut self, epoch: u32) {
        self.completed_time = 0.0;
        self.dropped_time.reset();
        self.epoch = Some(epoch);
        self.ticks_done = 0;
        self.faulted = false;
        self.cap_hit = false;
        self.clock_nonfinite = false;
        self.last_stats = None;
    }
}

impl Primitive for LiquidState {
    fn take_substep_restart_request(&mut self) -> bool {
        std::mem::take(&mut self.identity_reset)
    }

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.identity_ops.prepare(device);
        if self.record_copy.is_none() {
            self.record_copy = Some(RecordCopy::new(device));
        }
    }

    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &[
            "in",
            "stats_in",
            "identity_in",
            "clock_status_in",
            "whitewater_pool_in",
            "whitewater_state_in",
            "whitewater_counts_in",
            "foam_particles_in",
            "bubble_particles_in",
            "spray_particles_in",
            "dust_particles_in",
            "interior_in",
            "interior_tiles_in",
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
            "interior",
            "interior_tiles",
        ]
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "identity"
                | "whitewater_pool"
                | "whitewater_state"
                | "whitewater_counts"
                | "foam_particles"
                | "bubble_particles"
                | "spray_particles"
                | "dust_particles"
                | "interior"
                | "interior_tiles"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "identity" => self.identity.as_ref(),
            "whitewater_pool" => self.whitewater_pool.as_ref(),
            "whitewater_state" => self.whitewater_state.as_ref(),
            "whitewater_counts" => self.whitewater_counts.as_ref(),
            "foam_particles" => self.foam_particles.as_ref(),
            "bubble_particles" => self.bubble_particles.as_ref(),
            "spray_particles" => self.spray_particles.as_ref(),
            "dust_particles" => self.dust_particles.as_ref(),
            "interior" => self.interior.as_ref(),
            "interior_tiles" => self.interior_tiles.as_ref(),
            _ => None,
        }
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(LIQUID_STATE_PORTS)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "out" => input_capacities
                .iter()
                .find(|(p, _)| *p == "seed")
                .map(|&(_, n)| n),
            "stats" => Some(LIQUID_STATS_WORDS),
            "identity" => Some((IDENTITY_BYTES / 4) as u32),
            "clock_status" => Some(8),
            // Provided storage: a one-record hint, sized at run time.
            "whitewater_pool" | "whitewater_state" | "whitewater_counts" | "foam_particles"
            | "bubble_particles" | "spray_particles" | "dust_particles" | "interior"
            | "interior_tiles" => Some(1),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let count = whole(ctx.scalar_or_param("count", 0.0));
        let capacity = ctx
            .scalar_or_param("whitewater_capacity", WHITEWATER_DEFAULT_CAPACITY as f32)
            .round();
        if ctx.inputs.slot("whitewater_pool_in").is_some()
            && !(1.0..=WHITEWATER_MAX_CAPACITY as f32).contains(&capacity)
        {
            self.pending = 0;
            ctx.error(format!("Liquid State: whitewater capacity {capacity} is outside 1 to {WHITEWATER_MAX_CAPACITY}"));
            return;
        }
        let whitewater_capacity = capacity as u32;
        let ticks = whole(ctx.scalar_or_param("ticks", 0.0));
        self.submitted_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let target_time = ctx
            .inputs
            .scalar("target_time")
            .and_then(|value| match value {
                ParamValue::Float(time) => Some(f64::from(time)),
                _ => None,
            });
        let dropped_seconds = f64::from(ctx.scalar_or_param("dropped_seconds", 0.0));
        let epoch = whole(ctx.scalar_or_param("epoch", 0.0));
        let seed = ctx.inputs.array("seed");
        let out = ctx.outputs.array("out");
        let stats = ctx.outputs.array("stats");
        let mut refused = None;
        let whitewater_active: [bool; WHITEWATER_RESULT_COUNT] = std::array::from_fn(|i| {
            ctx.inputs
                .slot(RESULTS[WHITEWATER_RESULT_START + i].capture)
                .is_some()
        });
        let live_recovery = target_time.is_some() && !ctx.sim_step.offline();
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let recover = self.poll_readbacks(clock.as_ref(), live_recovery);
        if self.identity_reset && self.epoch == Some(epoch) {
            // The executor restarts the domain clock (and coupled rigid state).
            // Until its fresh epoch arrives, the allocator refuses all births
            // and the publisher keeps the last accepted frame.
            self.pending = 0;
            return;
        }
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let zero_stats = self
            .zero_stats
            .get_or_insert_with(|| gpu.device.create_buffer(stats_bytes, BufferUse::Gpu))
            .clone();
        let old_whitewater_capacity = self.whitewater_capacity;
        let whitewater_resized = match self.ensure_whitewater_buffers(
            gpu.device,
            whitewater_active,
            whitewater_capacity,
        ) {
            Ok(resized) => resized,
            Err(error) => {
                refused = Some(format!(
                    "Liquid State: whitewater buffers could not be allocated: {error}"
                ));
                false
            }
        };
        let whitewater_reset = whitewater_resized || old_whitewater_capacity != whitewater_capacity;

        let epoch_reset = self.epoch != Some(epoch) || self.identity_reset;
        // Recovery increments on the GPU, after all submitted births. Retired
        // CPU metadata may be older than the allocator currently on the queue.
        let seed_identity_epoch = if self.epoch != Some(epoch) {
            0
        } else {
            u32::MAX
        };
        if epoch_reset {
            self.reset_epoch(epoch);
        }
        let identity = self
            .identity
            .get_or_insert_with(|| gpu.device.create_buffer(IDENTITY_BYTES, BufferUse::Upload));
        if epoch_reset {
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * std::mem::size_of::<FluidParticle>() as u64)
                    .min(seed.size)
                    .min(out.size);
                if bytes > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                }
            }
            if let Some(out) = out {
                self.identity_ops.seed(
                    gpu.native_enc,
                    out,
                    identity,
                    count.min((out.size / 32) as u32),
                    seed_identity_epoch,
                );
            }
            if let Some(stats) = stats {
                gpu.native_enc.clear_buffer(&zero_stats);
                gpu.native_enc.copy_buffer_to_buffer(
                    &zero_stats,
                    stats,
                    stats_bytes.min(stats.size),
                );
            }
        }
        if epoch_reset || whitewater_reset {
            if let (Some(template), Some(pool)) = (&self.whitewater_empty, &self.whitewater_pool) {
                gpu.native_enc
                    .copy_buffer_to_buffer(template, pool, template.size.min(pool.size));
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
            // Replace poisoned state in encoder order without reanchoring time.
            // A numerical fault must not latch a show stop until manual Reset.
            if let (Some(seed), Some(out)) = (seed, out) {
                let bytes = (u64::from(count) * size_of::<FluidParticle>() as u64)
                    .min(seed.size)
                    .min(out.size);
                gpu.native_enc.copy_buffer_to_buffer(seed, out, bytes);
                self.identity_ops.seed(
                    gpu.native_enc,
                    out,
                    identity,
                    count.min((out.size / 32) as u32),
                    seed_identity_epoch,
                );
            }
        }
        self.identity_reset = false;
        if let Some(target) = target_time {
            let dropped_time = &mut self.dropped_time;
            ctx.sim_metrics.record(|metrics| {
                dropped_time.record(
                    metrics,
                    target,
                    self.completed_time,
                    dropped_seconds,
                    self.cap_hit,
                    self.faulted || self.clock_nonfinite,
                )
            });
        }

        self.pending = if refused.is_some() || (self.faulted && !live_recovery) {
            0
        } else {
            ticks
        };
        self.captures = 0;
        ctx.outputs.set_scalar(
            "fault",
            ParamValue::Float(if self.faulted { 1.0 } else { 0.0 }),
        );
        if self.faulted && self.last_stats.is_some_and(|s| s.nonfinite > 0) {
            ctx.error(if live_recovery {
                "Liquid State: non-finite values detected; reseeding water while the show continues"
            } else {
                "Liquid State: a tick produced non-finite values; the liquid is halted until Reset"
            });
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
        true
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A body that writes fresh storage is accepted by copy. The particles
        // copy only the slots in use, which the captured identity counts.
        if let (Some(candidate), Some(state)) = (ctx.inputs.array("in"), ctx.outputs.array("out"))
            && !candidate.ptr_eq(state)
        {
            let record = ctx
                .inputs
                .array("identity_in")
                .or(self.identity.as_ref())
                .expect("the state's identity exists once its run has begun");
            let copy = self
                .record_copy
                .as_ref()
                .expect("liquid state pipelines built by prepare_pipelines at install");
            copy.encode(ctx.gpu_encoder().native_enc, record, candidate, state);
        }
        for (candidate, state) in [
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
            if let (Some(candidate), Some(state)) =
                (ctx.inputs.array(candidate), ctx.outputs.array(state))
                && !candidate.ptr_eq(state)
            {
                let size = candidate.size.min(state.size);
                ctx.gpu_encoder()
                    .native_enc
                    .copy_buffer_to_buffer(candidate, state, size);
            }
        }
        // The interior and its table follow the step's pool: held at the
        // candidate's size, so a reader takes the pool from the length.
        for (candidate, port) in [
            ("interior_in", "interior"),
            ("interior_tiles_in", "interior_tiles"),
        ] {
            let held = if port == "interior" {
                &mut self.interior
            } else {
                &mut self.interior_tiles
            };
            let Some(candidate) = ctx.inputs.array(candidate) else {
                *held = None;
                continue;
            };
            let gpu = ctx.gpu_encoder();
            if held.as_ref().is_none_or(|held| held.size != candidate.size) {
                *held = None;
                match create_shared_buffer(gpu.device, candidate.size) {
                    Ok(buffer) => *held = Some(buffer),
                    Err(error) => {
                        ctx.error(format!(
                            "Liquid State: the {port} could not be held: {error}"
                        ));
                        continue;
                    }
                }
            }
            if let Some(held) = held.as_ref() {
                gpu.native_enc
                    .copy_buffer_to_buffer(candidate, held, candidate.size);
            }
        }
        self.captures += 1;
        if self.captures != self.pending {
            return;
        }
        self.ticks_done += u64::from(self.pending);
        let Some(stats) = ctx.outputs.array("stats") else {
            return;
        };
        let Some(epoch) = self.epoch else { return };
        let clock_status = ctx.inputs.array("clock_status_in");
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let stats_bytes = u64::from(LIQUID_STATS_WORDS) * 4;
        let complete = |slot: &ReadbackSlot| {
            !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp))
        };
        let index = match self.readback.iter().position(complete) {
            Some(index) => index,
            None if self.readback.len() < READBACK_SLOTS => {
                self.readback.push(ReadbackSlot {
                    buffer: gpu.device.create_buffer(stats_bytes, BufferUse::Readback),
                    identity: gpu
                        .device
                        .create_buffer(IDENTITY_BYTES, BufferUse::Readback),
                    clock_status: gpu.device.create_buffer(32, BufferUse::Readback),
                    stamp: 0,
                    epoch,
                    pending: false,
                    endpoint: 0.0,
                });
                self.readback.len() - 1
            }
            // Every slot is still in flight: skip this frame's readback; live
            // never waits on the GPU.
            None => return,
        };
        let slot = &mut self.readback[index];
        gpu.native_enc
            .copy_buffer_to_buffer(stats, &slot.buffer, stats_bytes.min(stats.size));
        if let Some(identity) = self.identity.as_ref() {
            gpu.native_enc
                .copy_buffer_to_buffer(identity, &slot.identity, IDENTITY_BYTES);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::exec::effect_node::EffectNode;

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
        assert_eq!(LIQUID_STATE_PORTS.iteration_scalars, ["tick_index"]);
    }

    #[test]
    fn liquid_state_whitewater_results_exclude_clock_and_include_dust() {
        let captures: Vec<_> = RESULTS
            [WHITEWATER_RESULT_START..WHITEWATER_RESULT_START + WHITEWATER_RESULT_COUNT]
            .iter()
            .map(|r| r.capture)
            .collect();
        assert_eq!(
            captures,
            [
                "whitewater_pool_in",
                "whitewater_state_in",
                "whitewater_counts_in",
                "foam_particles_in",
                "bubble_particles_in",
                "spray_particles_in",
                "dust_particles_in"
            ]
        );
    }
}

mod record_copy;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
