//! GPU-local FLIP timing and CFL scheduling.
//!
//! This is an internal scheduler helper for the FLIP domain.  It is not a
//! catalog atom: the reduction passes are deliberately barriered and are
//! submitted as one ordered chain through `manifold-gpu`.  The aggregate and
//! plan remain on the GPU, so the content thread never waits for a current
//! velocity maximum.
//!
//! The speed and final-step rules are a direct port of FLIP Fluids'
//! `FluidSimulation::_calculateNextTimeStep` and `nextUpdateTimeStep`
//! (`crates/manifold-fluids/native/flip_engine/fluidsimulation.cpp`, lines
//! 11088--11194 and 11430--11470), with the endpoint velocity bound from
//! `rigidfluidcoupling.cpp::pointSpeed`. Ryan L. Guy and Dennis Fassbaender, MIT; see
//! `THIRD_PARTY_NOTICES.md`.  In particular, the last allowed live substep
//! owns all remaining frame time.  A cap is a status bit, never an exception.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

manifold_core::testkit_visible! { pub(crate) const SHADER: &str = include_str!("shaders/gpu_flip_clock.wgsl"); }
const WORKGROUP: u32 = 64;
/// The authored GPU FLIP parameter range has at most 64 frame steps; the
/// histogram is exactly that existing range, not a population/quality cap.
const HISTOGRAM_BINS: u32 = 64;

/// Flags carried by [`GpuFlipClockParams::flags`].
pub mod flags {
    /// Use predicted source speed for the first substep.
    pub const FIRST_SUBSTEP: u32 = 1 << 0;
    /// Include eligible obstacle speed in the maximum.
    pub const FLUID_PRESENT_OR_GENERATING: u32 = 1 << 1;
    /// Enable the surface-tension CFL restriction.
    pub const SURFACE_TENSION: u32 = 1 << 2;
    /// Enable the source-colour mixing restriction.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub const COLOR_MIXING: u32 = 1 << 3;
}

/// Per-frame values consumed by the GPU scheduler.  This layout is also the
/// `ClockParams` uniform in `gpu_flip_clock.wgsl`; keep it 16-byte grouped.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuFlipClockParams {
    pub frame_duration: f32,
    pub cell_size: f32,
    pub cfl: f32,
    pub surface_condition: f32,
    pub surface_constant: f32,
    pub color_mixing_rate: f32,
    /// The numerical steps the tick encoded slots for, 0 for
    /// `max_frame_steps`: the last of them takes all remaining frame time.
    /// The marker speed limit still measures against `max_frame_steps`.
    pub encoded_steps: u32,
    /// One Sim Rate interval of simulated time for a live frame, or 0 to
    /// measure the marker speed limit against the whole frame (export). The
    /// kernel uses the shorter of the two, so a late live span never lowers
    /// the limit and loaded playback removes the same outliers export does.
    pub limit_interval: f32,
    pub min_frame_steps: u32,
    pub max_frame_steps: u32,
    pub flags: u32,
    pub interval_sequence: u32,
    /// Constant acceleration added to predicted source velocity.
    pub constant_force: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EventFieldParams {
    origin_spacing: [f32; 4],
    nodes_stride: [u32; 4],
}

impl GpuFlipClockParams {
    /// Bytes for an inline manifold-gpu uniform binding; this does not
    /// allocate.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

/// Inputs to one scheduling dispatch.  Buffers are persistent solver-owned
/// GPU buffers.  Body counts may be any value that fits the corresponding
/// buffer; there is no quality or population cap in this helper.
pub struct GpuFlipClockInputs<'a> {
    /// `FluidParticle` records (32-byte packed layout).  A zero radius is an
    /// unused slot and is excluded by the reduction shader.
    pub marker_particles: &'a GpuBuffer,
    /// The marker slots in use, on the GPU: a record `[groups, 1, 1, n]` with
    /// `n` at word 3 (a step's slot record). The marker passes cover the first
    /// `n` slots, capped at the capacity the clock was created with, and read
    /// none past.
    pub marker_count: &'a GpuBuffer,
    pub obstacle_vertices: &'a GpuBuffer,
    pub obstacle_count: u32,
    pub source_vertices: &'a GpuBuffer,
    pub source_count: u32,
    /// Interior live-simulation boundaries. Each record is
    /// `(relative_seconds, impulse_lattice_index, 0, 0)`.
    pub live_hits: &'a GpuBuffer,
    pub live_hit_count: u32,
    /// The packed impulse lattices indexed by a live hit. A zero stride
    /// disables event-aware velocity prediction.
    pub event_impulses: &'a GpuBuffer,
    pub impulse_stride: u32,
    pub impulse_nodes: [u32; 3],
    pub impulse_origin: [f32; 3],
    pub impulse_spacing: f32,
    /// Current tick's coupled body rows. The buffer binding is offset to the
    /// first body row, so coupled vertex `velocity.w` is row + 1.
    pub body_rows: &'a GpuBuffer,
    pub body_rows_offset: u64,
    /// Per-body reaction impulses accumulated by the pressure solve.
    pub body_reaction: &'a GpuBuffer,
}

/// The GPU-resident result consumed by the FLIP step.  The first record is a
/// `GpuFlipClockPlan` in shader layout; callers bind this buffer directly to
/// later kernels.  Reading it back is test-only and is never part of the live
/// path.
#[derive(Clone, Copy)]
pub struct GpuFlipClockPlan<'a> {
    buffer: &'a GpuBuffer,
}

impl<'a> GpuFlipClockPlan<'a> {
    /// Buffer binding for a downstream FLIP step.
    #[inline]
    pub fn buffer(&self) -> &'a GpuBuffer {
        self.buffer
    }
}

/// Persistent reduction and scheduling resources.  Construct this when the
/// FLIP domain is (re)allocated; [`Self::dispatch`] only encodes GPU work and
/// does not allocate.
pub struct GpuFlipClock {
    marker_args: GpuComputePipeline,
    marker_reduce: GpuComputePipeline,
    marker_partial: GpuComputePipeline,
    body_reduce: GpuComputePipeline,
    partial_reduce: GpuComputePipeline,
    marker_classify: GpuComputePipeline,
    marker_finalize: GpuComputePipeline,
    marker_remove: GpuComputePipeline,
    begin: GpuComputePipeline,
    schedule: GpuComputePipeline,
    scratch_a: GpuBuffer,
    scratch_b: GpuBuffer,
    marker_result: GpuBuffer,
    obstacle_result: GpuBuffer,
    source_result: GpuBuffer,
    marker_histogram: GpuBuffer,
    marker_outliers: GpuBuffer,
    plan: GpuBuffer,
    /// `[groups, 1, 1, n]` for the marker passes of the phase in flight
    /// (`marker_args`): level 0, classify and remove dispatch from it.
    marker_triple: GpuBuffer,
    marker_capacity: u32,
    /// Groups of each later marker reduction level, for `marker_capacity`
    /// slots; the last is one group and writes the aggregate.
    marker_partial_groups: Vec<u32>,
}

/// Groups of the marker reduction's later levels for `capacity` slots: level 0
/// leaves `ceil(capacity / 64)` partials and each level reduces the one before,
/// 64 to a group, until a single group holds what is left.
fn marker_partial_groups(capacity: u32) -> Vec<u32> {
    let mut groups = Vec::new();
    let mut partials = capacity.max(1).div_ceil(WORKGROUP);
    while partials > 1 {
        partials = partials.div_ceil(WORKGROUP);
        groups.push(partials);
    }
    groups
}

impl GpuFlipClock {
    /// Persistent storage held by the clock for the supplied populations.
    /// This is shared with extent admission so the scheduler cannot hide
    /// scratch or telemetry storage from the domain budget.
    #[cfg(any(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub(crate) fn held_bytes(
        marker_capacity: u32,
        obstacle_capacity: u32,
        source_capacity: u32,
    ) -> u64 {
        let max_capacity = marker_capacity
            .max(obstacle_capacity)
            .max(source_capacity)
            .max(1);
        let partial_capacity = u64::from(max_capacity.div_ceil(WORKGROUP).max(1));
        // Both vec4 reduction scratch buffers, three maxima, the histogram,
        // the outlier counters, the plan and the marker dispatch triple.
        2 * partial_capacity * 16 + 16 * 3 + u64::from(HISTOGRAM_BINS) * 4 + 16 + 48 + 16
    }

    /// Create resources sized from the largest input populations.  The caller
    /// owns the population sizes, so this imposes no solver-side cap.
    pub fn new(
        device: &GpuDevice,
        marker_capacity: u32,
        obstacle_capacity: u32,
        source_capacity: u32,
    ) -> Self {
        let max_capacity = marker_capacity
            .max(obstacle_capacity)
            .max(source_capacity)
            .max(1);
        let partial_capacity = max_capacity.div_ceil(WORKGROUP).max(1);
        let scratch_bytes = u64::from(partial_capacity) * 16;
        Self {
            marker_args: device.create_compute_pipeline(
                SHADER,
                "marker_args",
                "flip-clock-marker-args",
            ),
            marker_reduce: device.create_compute_pipeline(
                SHADER,
                "reduce_marker",
                "flip-clock-marker",
            ),
            marker_partial: device.create_compute_pipeline(
                SHADER,
                "reduce_partial_marker",
                "flip-clock-marker-partial",
            ),
            body_reduce: device.create_compute_pipeline(SHADER, "reduce_body", "flip-clock-body"),
            partial_reduce: device.create_compute_pipeline(
                SHADER,
                "reduce_partial",
                "flip-clock-partial",
            ),
            marker_classify: device.create_compute_pipeline(
                SHADER,
                "classify_marker",
                "flip-clock-marker-classify",
            ),
            marker_finalize: device.create_compute_pipeline(
                SHADER,
                "finalize_marker_limit",
                "flip-clock-marker-finalize",
            ),
            marker_remove: device.create_compute_pipeline(
                SHADER,
                "remove_marker_particles",
                "flip-clock-marker-remove",
            ),
            begin: device.create_compute_pipeline(SHADER, "begin_frame", "flip-clock-begin"),
            schedule: device.create_compute_pipeline(SHADER, "schedule", "flip-clock-schedule"),
            scratch_a: device.create_buffer(scratch_bytes, manifold_gpu::BufferUse::Gpu),
            scratch_b: device.create_buffer(scratch_bytes, manifold_gpu::BufferUse::Gpu),
            marker_result: device.create_buffer(16, manifold_gpu::BufferUse::Gpu),
            obstacle_result: device.create_buffer(16, manifold_gpu::BufferUse::Gpu),
            source_result: device.create_buffer(16, manifold_gpu::BufferUse::Gpu),
            marker_histogram: device.create_buffer(u64::from(HISTOGRAM_BINS) * 4, manifold_gpu::BufferUse::Gpu),
            marker_outliers: device.create_buffer(16, manifold_gpu::BufferUse::Gpu),
            plan: device.create_buffer(48, manifold_gpu::BufferUse::Gpu),
            marker_triple: device.create_buffer(16, manifold_gpu::BufferUse::Gpu),
            marker_capacity,
            marker_partial_groups: marker_partial_groups(marker_capacity),
        }
    }

    /// Encode current-state reductions and exact FLIP scheduling.  GPU
    /// dispatch ordering supplies the inter-pass resource dependency; every
    /// per-workgroup reduction also has explicit WGSL barriers.
    /// Begin a frame. This is the only CPU-supplied time cursor: subsequent
    /// [`Self::dispatch`] calls read and advance the GPU-resident plan.
    pub fn begin_frame(&self, encoder: &mut GpuEncoder, params: &GpuFlipClockParams) {
        encoder.dispatch_compute(
            &self.begin,
            &[
                GpuBinding::Buffer {
                    binding: 14,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 15,
                    data: params.as_bytes(),
                },
            ],
            [1, 1, 1],
            "flip-clock-begin",
        );
        encoder.compute_memory_barrier_buffers();
    }

    pub fn dispatch<'a>(
        &'a self,
        encoder: &mut GpuEncoder,
        inputs: GpuFlipClockInputs<'_>,
        params: &GpuFlipClockParams,
    ) -> GpuFlipClockPlan<'a> {
        let event_params = EventFieldParams {
            origin_spacing: [
                inputs.impulse_origin[0],
                inputs.impulse_origin[1],
                inputs.impulse_origin[2],
                inputs.impulse_spacing,
            ],
            nodes_stride: [
                inputs.impulse_nodes[0],
                inputs.impulse_nodes[1],
                inputs.impulse_nodes[2],
                inputs.impulse_stride,
            ],
        };
        // A population that skips its reduction must not reuse a previous
        // frame's aggregate: this zeroes the obstacle and source ones. The
        // marker reduction always runs and writes its own.
        self.marker_args(
            encoder,
            inputs.marker_count,
            ReductionPhase::Scheduling,
            true,
        );
        self.reduce_marker(
            encoder,
            inputs.marker_particles,
            inputs.live_hits,
            inputs.live_hit_count,
            inputs.event_impulses,
            &event_params,
            params,
        );
        if inputs.obstacle_count > 0 {
            let obstacle = self.reduce_body(
                encoder,
                inputs.obstacle_vertices,
                inputs.obstacle_count,
                false,
                inputs.body_rows,
                inputs.body_rows_offset,
                inputs.body_reaction,
                params,
            );
            encoder.copy_buffer_to_buffer(obstacle, &self.obstacle_result, 16);
        }
        // The native engine predicts source speed only before the first
        // simulation step. Later intervals use current marker velocities.
        if inputs.source_count > 0 && params.flags & flags::FIRST_SUBSTEP != 0 {
            let source = self.reduce_body(
                encoder,
                inputs.source_vertices,
                inputs.source_count,
                true,
                inputs.body_rows,
                inputs.body_rows_offset,
                inputs.body_reaction,
                params,
            );
            encoder.copy_buffer_to_buffer(source, &self.source_result, 16);
        }
        encoder.compute_memory_barrier_buffers();
        let hit_params = [inputs.live_hit_count, 0, 0, 0];

        encoder.dispatch_compute(
            &self.schedule,
            &[
                GpuBinding::Buffer {
                    binding: 11,
                    buffer: &self.marker_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 12,
                    buffer: &self.obstacle_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 13,
                    buffer: &self.source_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 14,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 15,
                    data: params.as_bytes(),
                },
                GpuBinding::Buffer {
                    binding: 16,
                    buffer: inputs.live_hits,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 17,
                    data: bytemuck::bytes_of(&hit_params),
                },
                GpuBinding::Buffer {
                    binding: 18,
                    buffer: &self.marker_histogram,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 19,
                    buffer: &self.marker_outliers,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-schedule",
        );
        encoder.compute_memory_barrier_buffers();
        GpuFlipClockPlan { buffer: &self.plan }
    }

    /// Histogram and outlier counts of the markers `marker_args` covers.
    fn classify_marker(
        &self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        params: &GpuFlipClockParams,
    ) {
        encoder.dispatch_compute_indirect(
            &self.marker_classify,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 22,
                    data: params.as_bytes(),
                },
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: &self.marker_histogram,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: &self.marker_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 20,
                    buffer: &self.marker_outliers,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 30,
                    buffer: &self.marker_triple,
                    offset: 0,
                },
            ],
            &self.marker_triple,
            0,
            "flip-clock-marker-classify",
        );
        encoder.compute_memory_barrier_buffers();
    }

    fn finalize_marker_limit(&self, encoder: &mut GpuEncoder, params: &GpuFlipClockParams) {
        encoder.dispatch_compute(
            &self.marker_finalize,
            &[
                GpuBinding::Buffer {
                    binding: 11,
                    buffer: &self.marker_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 14,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 15,
                    data: params.as_bytes(),
                },
                GpuBinding::Buffer {
                    binding: 18,
                    buffer: &self.marker_histogram,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 19,
                    buffer: &self.marker_outliers,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-marker-finalize",
        );
    }

    /// Recompute the native marker threshold from the post-step velocities and
    /// remove over-limit particles in place. This does not touch the clock
    /// cursor; it uses the accepted frame interval, as native _currentFrameDeltaTime.
    /// `count` is the record of the slots in use (`GpuFlipClockInputs::marker_count`):
    /// a particle at or past its `n` is neither measured nor removed.
    pub(crate) fn remove_extreme(
        &self,
        encoder: &mut GpuEncoder,
        particles: &GpuBuffer,
        count: &GpuBuffer,
        params: &GpuFlipClockParams,
    ) {
        let empty_hits = self.marker_histogram.clone();
        let empty_params = EventFieldParams {
            origin_spacing: [0.0, 0.0, 0.0, 1.0],
            nodes_stride: [2, 2, 2, 0],
        };
        self.marker_args(encoder, count, ReductionPhase::Cleanup, false);
        self.reduce_marker(
            encoder,
            particles,
            &empty_hits,
            0,
            &empty_hits,
            &empty_params,
            params,
        );
        encoder.clear_buffer(&self.marker_histogram);
        encoder.clear_buffer(&self.marker_outliers);
        self.classify_marker(encoder, particles, params);
        self.finalize_marker_limit(encoder, params);
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute_indirect(
            &self.marker_remove,
            &[
                GpuBinding::Buffer {
                    binding: 25,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 30,
                    buffer: &self.marker_triple,
                    offset: 0,
                },
            ],
            &self.marker_triple,
            0,
            "flip-clock-marker-remove",
        );
    }

    /// One thread: the marker passes' dispatch triple for `phase`, from the
    /// record of the slots in use. With `clear_aggregates` it also zeroes the
    /// obstacle and source aggregates.
    fn marker_args(
        &self,
        encoder: &mut GpuEncoder,
        record: &GpuBuffer,
        phase: ReductionPhase,
        clear_aggregates: bool,
    ) {
        let words = [
            self.marker_capacity,
            phase as u32,
            u32::from(clear_aggregates),
            0,
        ];
        encoder.dispatch_compute(
            &self.marker_args,
            &[
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 31,
                    buffer: &self.marker_triple,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 32,
                    buffer: record,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 33,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Buffer {
                    binding: 34,
                    buffer: &self.obstacle_result,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 35,
                    buffer: &self.source_result,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-marker-args",
        );
        encoder.compute_memory_barrier_buffers();
    }

    /// The maximum marker speed over the slots `marker_args` covers, into
    /// `marker_result`. Level 0 dispatches from the triple; the later levels
    /// are shaped for the capacity and take their lengths from `n`, so the
    /// chain is the same whatever the population and the last level always
    /// writes the aggregate (zero for an empty one).
    fn reduce_marker(
        &self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        live_hits: &GpuBuffer,
        live_hit_count: u32,
        event_impulses: &GpuBuffer,
        event_params: &EventFieldParams,
        clock_params: &GpuFlipClockParams,
    ) {
        let hit_params = [live_hit_count, 0, 0, 0];
        let later = self.marker_partial_groups.len();
        let first = if later == 0 {
            &self.marker_result
        } else {
            &self.scratch_a
        };
        encoder.dispatch_compute_indirect(
            &self.marker_reduce,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: first,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 16,
                    buffer: live_hits,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 17,
                    data: bytemuck::bytes_of(&hit_params),
                },
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 27,
                    buffer: event_impulses,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 15,
                    data: clock_params.as_bytes(),
                },
                GpuBinding::Bytes {
                    binding: 26,
                    data: bytemuck::bytes_of(event_params),
                },
                GpuBinding::Buffer {
                    binding: 30,
                    buffer: &self.marker_triple,
                    offset: 0,
                },
            ],
            &self.marker_triple,
            0,
            "flip-clock-marker-reduce",
        );
        encoder.compute_memory_barrier_buffers();
        for (pass, &groups) in self.marker_partial_groups.iter().enumerate() {
            let level = pass as u32 + 1;
            let (src, dst) = if level.is_multiple_of(2) {
                (&self.scratch_b, &self.scratch_a)
            } else {
                (&self.scratch_a, &self.scratch_b)
            };
            let dst = if pass + 1 == later {
                &self.marker_result
            } else {
                dst
            };
            let level_words = [level, 0, 0, 0];
            encoder.dispatch_compute(
                &self.marker_partial,
                &[
                    GpuBinding::Buffer {
                        binding: 8,
                        buffer: src,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 9,
                        buffer: dst,
                        offset: 0,
                    },
                    GpuBinding::Bytes {
                        binding: 36,
                        data: bytemuck::cast_slice(&level_words),
                    },
                    GpuBinding::Buffer {
                        binding: 30,
                        buffer: &self.marker_triple,
                        offset: 0,
                    },
                ],
                [groups, 1, 1],
                "flip-clock-marker-partial",
            );
            encoder.compute_memory_barrier_buffers();
        }
    }

    fn reduce_body<'a>(
        &'a self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        count: u32,
        source: bool,
        body_rows: &GpuBuffer,
        body_rows_offset: u64,
        body_reaction: &GpuBuffer,
        params: &GpuFlipClockParams,
    ) -> &'a GpuBuffer {
        debug_assert!(
            count > 0,
            "empty body populations retain the cleared aggregate"
        );
        let mode = u32::from(source);
        let mut n = count;
        let mut pass = 0u32;
        let reduce_data = reduce_bytes(n, mode, ReductionPhase::Scheduling);
        encoder.dispatch_compute(
            &self.body_reduce,
            &[
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &self.scratch_a,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 5,
                    data: &reduce_data,
                },
                GpuBinding::Bytes {
                    binding: 6,
                    data: params.as_bytes(),
                },
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 28,
                    buffer: body_rows,
                    offset: body_rows_offset,
                },
                GpuBinding::Buffer {
                    binding: 29,
                    buffer: body_reaction,
                    offset: 0,
                },
            ],
            [n.div_ceil(WORKGROUP), 1, 1],
            if source {
                "flip-clock-source-reduce"
            } else {
                "flip-clock-obstacle-reduce"
            },
        );
        encoder.compute_memory_barrier_buffers();
        n = n.div_ceil(WORKGROUP);
        while n > 1 {
            let (src, dst) = if pass.is_multiple_of(2) {
                (&self.scratch_a, &self.scratch_b)
            } else {
                (&self.scratch_b, &self.scratch_a)
            };
            let reduce_data = reduce_bytes(n, 0, ReductionPhase::Scheduling);
            encoder.dispatch_compute(
                &self.partial_reduce,
                &[
                    GpuBinding::Buffer {
                        binding: 8,
                        buffer: src,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 9,
                        buffer: dst,
                        offset: 0,
                    },
                    GpuBinding::Bytes {
                        binding: 10,
                        data: &reduce_data,
                    },
                    GpuBinding::Buffer {
                        binding: 24,
                        buffer: &self.plan,
                        offset: 0,
                    },
                ],
                [n.div_ceil(WORKGROUP), 1, 1],
                "flip-clock-body-partial",
            );
            encoder.compute_memory_barrier_buffers();
            n = n.div_ceil(WORKGROUP);
            pass += 1;
        }
        if pass.is_multiple_of(2) {
            &self.scratch_a
        } else {
            &self.scratch_b
        }
    }
}

#[inline]
fn reduce_bytes(count: u32, mode: u32, phase: ReductionPhase) -> [u8; 16] {
    let words = [count, mode, phase as u32, 0];
    // A stack-owned inline payload; manifold-gpu consumes it during encode.
    bytemuck::cast(words)
}

/// Scheduling precedes `schedule`; cleanup follows the accepted step, whose
/// duration remains positive even when it consumes the interval remainder.
#[derive(Clone, Copy)]
#[repr(u32)]
enum ReductionPhase {
    Scheduling = 0,
    Cleanup = 1,
}

#[cfg(test)]
// The shader implements the native epsilon/ceil schedule in f32. Widening
// its inputs before the arithmetic can cross an integer ceil boundary.
fn expected_dt(
    p: &GpuFlipClockParams,
    speed: f64,
    restrictions: manifold_physics::stepping::CflRestrictions,
) -> f32 {
    let mut limit = p.cfl * p.cell_size / (speed as f32 + 1e-6);
    if let Some((condition, constant)) = restrictions.surface_tension {
        limit = limit.min(
            condition as f32
                * (p.cell_size * p.cell_size * p.cell_size).sqrt()
                * (1.0 / (constant as f32 + 1e-6)).sqrt(),
        );
    }
    if let Some(rate) = restrictions.color_mixing_rate {
        limit = limit.min(1.0 / (rate as f32 + 1e-6));
    }
    p.frame_duration / (p.frame_duration / limit).ceil().max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_flip_clock_epsilon_ceil_and_cap_cpu_proof() {
        use manifold_physics::Seconds;
        use manifold_physics::stepping::{CflRestrictions, LiveStepSchedule};
        let p = GpuFlipClockParams {
            frame_duration: 0.1,
            cell_size: 0.1,
            cfl: 5.0,
            surface_condition: 0.0,
            surface_constant: 0.0,
            color_mixing_rate: 0.0,
            encoded_steps: 0,
            limit_interval: 0.1,
            min_frame_steps: 1,
            max_frame_steps: 6,
            flags: 0,
            interval_sequence: 0,
            constant_force: [0.0; 4],
        };
        // 0.5 / (10 + epsilon) is below 0.05: ceil requires three
        // subdivisions, not the two in the old reaction proof.
        assert_eq!(
            expected_dt(&p, 10.0, CflRestrictions::default()),
            p.frame_duration / 3.0
        );
        for speed in [0.0, 5.0, 10.0, 100.0, 1000.0] {
            let duration = expected_dt(&p, speed, CflRestrictions::default());
            let mut schedule =
                LiveStepSchedule::new(Seconds::ZERO, Seconds(f64::from(p.frame_duration)), 1, 6)
                    .value;
            let mut end = Seconds::ZERO;
            let mut capped = false;
            while let Some(step) = schedule.next(Seconds(f64::from(duration))).value {
                assert_eq!(step.interval.start, end);
                end = step.interval.end;
                capped |= step.hit_cap;
            }
            assert_eq!(end, Seconds(f64::from(p.frame_duration)));
            assert!(schedule.steps_taken() <= 6);
            assert_eq!(capped, speed >= 100.0);
        }
    }

    #[test]
    fn gpu_flip_clock_marker_levels_end_in_one_group() {
        // Level 0 leaves ceil(capacity / 64) partials; each later level
        // takes 64 to a group until one group holds the rest.
        for (capacity, groups) in [
            (0, vec![]),
            (1, vec![]),
            (64, vec![]),
            (65, vec![1]),
            (4_096, vec![1]),
            (4_097, vec![2, 1]),
            (70_000, vec![18, 1]),
            (u32::MAX, vec![1_048_576, 16_384, 256, 4, 1]),
        ] {
            assert_eq!(
                marker_partial_groups(capacity),
                groups,
                "capacity {capacity}"
            );
        }
    }

    #[test]
    fn gpu_flip_clock_shader_parses_and_validates_on_cpu() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("flip clock WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("flip clock WGSL validates");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use bytemuck::Zeroable;
    use manifold_node_engine::particles::FluidParticle;
    use manifold_physics::stepping::CflRestrictions;
    use manifold_water_liquid::bodies::GpuFlipBodyVertex;
    use manifold_water_liquid::bodies::LiquidBody;
    use std::mem::size_of;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
    struct PlanValue {
        dt: f32,
        elapsed: f32,
        remaining: f32,
        maximum_speed: f32,
        cap_hit: u32,
        nonfinite: u32,
        step_index: u32,
        pad: u32,
        numerical_end: f32,
        marker_limit: f32,
        pad0: u32,
        live_mode: u32,
    }

    fn params() -> GpuFlipClockParams {
        GpuFlipClockParams {
            frame_duration: 1.0,
            cell_size: 0.1,
            cfl: 5.0,
            surface_condition: 0.0,
            surface_constant: 0.0,
            color_mixing_rate: 0.0,
            encoded_steps: 0,
            limit_interval: 1.0,
            min_frame_steps: 1,
            max_frame_steps: 6,
            flags: 0,
            interval_sequence: 0,
            constant_force: [0.0; 4],
        }
    }

    fn particle(speed: f32) -> FluidParticle {
        FluidParticle {
            position_radius: [0.0, 0.0, 0.0, 1.0],
            velocity: [speed, 0.0, 0.0],
            id: 1,
        }
    }

    fn read_plan(buffer: &GpuBuffer) -> PlanValue {
        // Every caller waits for its copy fence before reading shared memory.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), 48) };
        bytemuck::pod_read_unaligned(bytes)
    }

    fn read_words(buffer: &GpuBuffer, count: usize) -> Vec<u32> {
        let bytes = unsafe {
            std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), count * size_of::<u32>())
        };
        bytemuck::cast_slice(bytes).to_vec()
    }

    /// A record of `n` slots in use as a step writes it: `[groups, 1, 1, n]`.
    fn slots_record(device: &GpuDevice, n: u32) -> GpuBuffer {
        let record = device.create_buffer(16, manifold_gpu::BufferUse::Upload);
        // SAFETY: a fresh shared buffer, no GPU work in flight.
        unsafe { record.write(0, bytemuck::cast_slice(&[n.div_ceil(256).max(1), 1, 1, n])) };
        record
    }

    fn original_marker_clock(device: &GpuDevice, capacity: u32) -> GpuFlipClock {
        // Frozen global-atomic classifier: independent of the production
        // workgroup implementation, with identical per-particle f32 math.
        const ORIGINAL: &str = r#"@compute @workgroup_size(64)
fn classify_marker(@builtin(global_invocation_id) gid: vec3<u32>) {
    if marker_clock_plan[0].live_mode == 0u || marker_clock_plan[0].step_dt <= 0.0 {
        return;
    }
    let idx = gid.x;
    if idx >= marker_triple[3] {
        return;
    }
    let particle = marker_values[idx];
    let v = particle.velocity;
    if !(particle.position_radius.w > 0.0 && finite3(v) && finite3(particle.position_radius.xyz)) {
        return;
    }
    let speed = length(v);
    if !finite1(speed) {
        return;
    }
    let speed_limit_step = marker_clock_params.cfl * marker_clock_params.cell_size /
        speed_limit_duration(marker_clock_params);
    let bin = min(u32(floor(speed / speed_limit_step)),
        max(marker_clock_params.max_frame_steps, 1u) - 1u);
    atomicAdd(&marker_histogram_atomic[bin], 1u);
    let maximum = partial_values[0].x;
    if speed >= 0.90 * maximum && speed < 0.99999 * maximum {
        atomicAdd(&marker_outliers_atomic[0], 1u);
    }
    if speed >= 0.99999 * maximum {
        atomicAdd(&marker_outliers_atomic[1], 1u);
    }
}
"#;
        let start = SHADER
            .find("fn classify_marker_lane(")
            .expect("classifier lane helper");
        let end = SHADER[start..]
            .find("\n// The frame the marker speed limit")
            .expect("classifier end")
            + start;
        let mut shader = SHADER.to_owned();
        shader.replace_range(start..end, ORIGINAL);
        let mut clock = GpuFlipClock::new(device, capacity, 1, 1);
        clock.marker_classify = device.create_compute_pipeline(
            &shader,
            "classify_marker",
            "marker-classify-original-proof",
        );
        clock
    }

    // Setup and readback use separate completed command buffers: total_ms
    // below measures only the classifier dispatch and its barrier. The
    // setup also derives the dispatch triple from the record.
    fn classify_probe(
        device: &GpuDevice,
        clock: &GpuFlipClock,
        input: (&GpuBuffer, &GpuBuffer),
        p: &GpuFlipClockParams,
        plan: &PlanValue,
        seed: Option<&[u32; 68]>,
        buffers: &[GpuBuffer; 4],
    ) -> (Vec<u32>, f64) {
        let (markers, record) = input;
        let [plan_seed, maximum_seed, histogram, outliers] = buffers;
        unsafe {
            plan_seed.write(0, bytemuck::bytes_of(plan));
            maximum_seed.write(0, bytemuck::cast_slice(&[32.0f32, 0.0, 0.0, 0.0]));
            if let Some(seed) = seed {
                histogram.write(0, bytemuck::cast_slice(&seed[..64]));
                outliers.write(0, bytemuck::cast_slice(&seed[64..]));
            }
        }
        let mut enc = device.create_encoder("marker-classify-proof-setup");
        enc.copy_buffer_to_buffer(plan_seed, &clock.plan, 48);
        enc.copy_buffer_to_buffer(maximum_seed, &clock.marker_result, 16);
        if seed.is_some() {
            enc.copy_buffer_to_buffer(histogram, &clock.marker_histogram, 256);
            enc.copy_buffer_to_buffer(outliers, &clock.marker_outliers, 16);
        } else {
            enc.clear_buffer(&clock.marker_histogram);
            enc.clear_buffer(&clock.marker_outliers);
        }
        clock.marker_args(&mut enc, record, ReductionPhase::Cleanup, false);
        enc.commit_and_wait().expect("GPU work completes");
        let mut enc = device.create_encoder("marker-classify-proof-dispatch");
        clock.classify_marker(&mut enc, markers, p);
        let timing = enc.commit_and_wait_profiled(device);
        let mut enc = device.create_encoder("marker-classify-proof-readback");
        enc.copy_buffer_to_buffer(&clock.marker_histogram, histogram, 256);
        enc.copy_buffer_to_buffer(&clock.marker_outliers, outliers, 16);
        enc.commit_and_wait().expect("GPU work completes");
        let mut words = read_words(histogram, 64);
        words.extend(read_words(outliers, 4));
        (words, timing.total_ms)
    }

    fn classify_buffers(device: &GpuDevice) -> [GpuBuffer; 4] {
        [48, 16, 256, 16].map(|bytes| device.create_buffer(bytes, manifold_gpu::BufferUse::Upload))
    }

    #[test]
    fn gpu_flip_clock_workgroup_marker_histogram_exact_proof() {
        let device = manifold_gpu::testkit::test_device();
        let original = original_marker_clock(&device, 257);
        let candidate = GpuFlipClock::new(&device, 257, 1, 1);
        let markers = device.create_buffer(257 * 32, manifold_gpu::BufferUse::Upload);
        let buffers = classify_buffers(&device);
        let active = PlanValue {
            dt: 0.25,
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        // Include adjacent f32 words at bin and both relative-outlier edges.
        // Exact parity is against the production GPU math, including sqrt.
        let edges = [
            0.0,
            f32::from_bits(0.5f32.to_bits() - 1),
            0.5,
            f32::from_bits(0.5f32.to_bits() + 1),
            31.75,
            32.0,
            f32::from_bits((0.90f32 * 32.0).to_bits() - 1),
            0.90 * 32.0,
            f32::from_bits((0.90f32 * 32.0).to_bits() + 1),
            f32::from_bits((0.99999f32 * 32.0).to_bits() - 1),
            0.99999 * 32.0,
            f32::from_bits((0.99999f32 * 32.0).to_bits() + 1),
        ];
        for count in [0, 1, 63, 64, 65, 257] {
            let record = slots_record(&device, count);
            for max_steps in [1, 6, 64] {
                let p = GpuFlipClockParams {
                    max_frame_steps: max_steps,
                    ..params()
                };
                for mixed in [false, true] {
                    let mut particles = vec![particle(0.25); count as usize];
                    if mixed {
                        for (i, marker) in particles.iter_mut().enumerate() {
                            *marker = particle(if i < edges.len() {
                                edges[i]
                            } else {
                                (i % 64) as f32 * 0.5 + 0.25
                            });
                            match i {
                                12 => marker.position_radius[3] = 0.0,
                                13 => marker.position_radius[3] = -1.0,
                                14 => marker.velocity[0] = f32::NAN,
                                15 => marker.velocity[0] = f32::INFINITY,
                                16 => marker.position_radius[0] = f32::NAN,
                                17 => marker.position_radius[1] = f32::INFINITY,
                                18 => marker.position_radius[3] = f32::NAN,
                                19 => marker.position_radius[3] = f32::INFINITY,
                                20 => marker.velocity[0] = f32::MAX,
                                _ => {}
                            }
                        }
                    }
                    if let Some(last) = particles.last_mut() {
                        *last = particle(32.0);
                    }
                    unsafe {
                        markers.write(0, bytemuck::cast_slice(&particles));
                    }
                    let mut previous = None;
                    for repeat in 0..2 {
                        let (old, _) = classify_probe(
                            &device,
                            &original,
                            (&markers, &record),
                            &p,
                            &active,
                            None,
                            &buffers,
                        );
                        let (new, _) = classify_probe(
                            &device,
                            &candidate,
                            (&markers, &record),
                            &p,
                            &active,
                            None,
                            &buffers,
                        );
                        assert_eq!(
                            old, new,
                            "count {count}, max_steps {max_steps}, mixed {mixed}, repeat {repeat}"
                        );
                        assert_eq!(&new[66..], &[0, 0], "unused outlier words stay clear");
                        if let Some(previous) = &previous {
                            assert_eq!(&new, previous, "cleared repeat");
                        }
                        previous = Some(new);
                    }
                }
            }
        }
        let seed = std::array::from_fn(|i| 0x1357_0000u32 + i as u32);
        let record = slots_record(&device, 65);
        for inactive in [
            PlanValue {
                live_mode: 0,
                ..active
            },
            PlanValue { dt: 0.0, ..active },
        ] {
            for clock in [&original, &candidate] {
                let (words, _) = classify_probe(
                    &device,
                    clock,
                    (&markers, &record),
                    &params(),
                    &inactive,
                    Some(&seed),
                    &buffers,
                );
                assert_eq!(
                    words, seed,
                    "inactive classifier preserves all seeded words"
                );
            }
        }
    }

    #[test]
    fn gpu_flip_clock_workgroup_marker_cleanup_exact_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clocks = [
            original_marker_clock(&device, 21_000),
            GpuFlipClock::new(&device, 21_000, 1, 1),
        ];
        let markers = [
            device.create_buffer(21_000 * 32, manifold_gpu::BufferUse::Upload),
            device.create_buffer(21_000 * 32, manifold_gpu::BufferUse::Upload),
        ];
        let plan_seed = device.create_buffer(48, manifold_gpu::BufferUse::Upload);
        let plans = [
            device.create_buffer(48, manifold_gpu::BufferUse::Upload),
            device.create_buffer(48, manifold_gpu::BufferUse::Upload),
        ];
        let buffers = classify_buffers(&device);
        // Same native budget/floor populations as the CPU policy oracle.
        for (count, duration) in [(300, 0.25), (300, 1.0), (21_000, 1.0)] {
            let mut particles = vec![particle(1.25); count as usize];
            for (i, marker) in particles.iter_mut().enumerate() {
                marker.id = i as u32 + 101;
            }
            for marker in particles.iter_mut().take(9) {
                marker.velocity[0] = 11.0;
            }
            particles[9].velocity[0] = 3.25;
            let record = slots_record(&device, count);
            for span in [1.0, 4.0] {
                let p = GpuFlipClockParams {
                    frame_duration: duration * span,
                    limit_interval: if span == 1.0 { 0.0 } else { duration },
                    ..params()
                };
                let plan = PlanValue {
                    dt: duration / 3.0,
                    remaining: duration * span,
                    live_mode: 1,
                    ..PlanValue::zeroed()
                };
                unsafe {
                    plan_seed.write(0, bytemuck::bytes_of(&plan));
                    for buffer in &markers {
                        buffer.write(0, bytemuck::cast_slice(&particles));
                    }
                }
                let mut outputs = Vec::new();
                for (i, clock) in clocks.iter().enumerate() {
                    let mut enc = device.create_encoder("marker-workgroup-cleanup-proof");
                    enc.copy_buffer_to_buffer(&plan_seed, &clock.plan, 48);
                    enc.compute_memory_barrier_buffers();
                    clock.remove_extreme(&mut enc, &markers[i], &record, &p);
                    enc.copy_buffer_to_buffer(&clock.plan, &plans[i], 48);
                    enc.copy_buffer_to_buffer(&clock.marker_histogram, &buffers[2], 256);
                    enc.copy_buffer_to_buffer(&clock.marker_outliers, &buffers[3], 16);
                    enc.commit_and_wait().expect("GPU work completes");
                    outputs.push((
                        read_words(&markers[i], count as usize * 8),
                        read_words(&plans[i], 12),
                        read_words(&buffers[2], 64),
                        read_words(&buffers[3], 4),
                    ));
                }
                assert_eq!(
                    outputs[0], outputs[1],
                    "{count} markers, {duration}s, {span}x span: particles/plan/histogram/outliers"
                );
            }
        }
    }

    #[test]
    fn gpu_flip_clock_inactive_reductions_preserve_scratch_and_final_cleanup() {
        // More than one workgroup exercises both the population and partial
        // guards. The same allocations then become empty without stale maxima.
        const COUNT: u32 = 512;
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, COUNT, COUNT, COUNT);
        let full = slots_record(&device, COUNT);
        let none = slots_record(&device, 0);
        let markers = device.create_buffer(u64::from(COUNT) * 32, manifold_gpu::BufferUse::Upload);
        let vertices = device.create_buffer(u64::from(COUNT) * 96, manifold_gpu::BufferUse::Upload);
        let plan_input = device.create_buffer(48, manifold_gpu::BufferUse::Upload);
        let plan_readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let aggregate_readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let scratch_readback = [
            device.create_buffer(clock.scratch_a.size, manifold_gpu::BufferUse::Upload),
            device.create_buffer(clock.scratch_b.size, manifold_gpu::BufferUse::Upload),
        ];
        let poison = device.create_buffer(clock.scratch_a.size, manifold_gpu::BufferUse::Upload);
        let poison_words = vec![0x7fc0_1234u32; (poison.size / 4) as usize];
        let mut particles = [particle(1.0); COUNT as usize];
        // A power-of-two speed keeps length's GPU square-root exact, so the
        // reduction and removal assertions can compare literal bits.
        particles[COUNT as usize - 1] = particle(1024.0);
        let vertex = GpuFlipBodyVertex {
            position: [0.0, 0.0, 0.0, 1.0],
            velocity: [4.0, 0.0, 0.0, 0.0],
            acceleration: [0.0; 4],
            angular_velocity: [0.0; 4],
            angular_acceleration: [0.0; 4],
            centroid: [0.0; 4],
        };
        // SAFETY: shared storage, before any work is submitted.
        unsafe {
            markers.write(0, bytemuck::cast_slice(&particles));
            vertices.write(0, bytemuck::cast_slice(&[vertex; COUNT as usize]));
            poison.write(0, bytemuck::cast_slice(&poison_words));
        }
        let p = params();
        let dispatch = |enc: &mut GpuEncoder, record: &GpuBuffer, count| {
            clock.dispatch(
                enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: record,
                    obstacle_vertices: &vertices,
                    obstacle_count: count,
                    source_vertices: &vertices,
                    source_count: count,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &vertices,
                    body_rows_offset: 0,
                    body_reaction: &vertices,
                },
                &p,
            );
        };
        let copy_scratch = |enc: &mut GpuEncoder| {
            for (scratch, readback) in [&clock.scratch_a, &clock.scratch_b]
                .into_iter()
                .zip(&scratch_readback)
            {
                enc.copy_buffer_to_buffer(scratch, readback, scratch.size);
            }
        };
        let assert_poison = || {
            for readback in &scratch_readback {
                assert_eq!(
                    read_words(readback, poison_words.len()),
                    poison_words,
                    "inactive reductions must not write either scratch buffer"
                );
            }
        };

        let mut enc = device.create_encoder("flip-clock previously nonempty populations");
        clock.begin_frame(&mut enc, &p);
        dispatch(&mut enc, &full, COUNT);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.copy_buffer_range(&clock.obstacle_result, 0, &aggregate_readback, 16, 16);
        enc.copy_buffer_range(&clock.source_result, 0, &aggregate_readback, 32, 16);
        enc.commit_and_wait().expect("GPU work completes");
        assert_eq!(read_plan(&plan_readback).maximum_speed, 1024.0);
        let aggregates = read_words(&aggregate_readback, 12);
        assert_eq!(f32::from_bits(aggregates[4]), 4.0);
        assert_eq!(
            aggregates[8], 0,
            "outside startup, the unused source aggregate stays cleared"
        );

        let mut enc = device.create_encoder("flip-clock populations become empty");
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_a, poison.size);
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_b, poison.size);
        clock.begin_frame(&mut enc, &p);
        dispatch(&mut enc, &none, 0);
        for (index, aggregate) in [
            &clock.marker_result,
            &clock.obstacle_result,
            &clock.source_result,
        ]
        .into_iter()
        .enumerate()
        {
            enc.copy_buffer_range(aggregate, 0, &aggregate_readback, index as u64 * 16, 16);
        }
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        // The poisoned scratch is stale data an empty marker reduction must
        // not read: every aggregate is zero.
        assert_eq!(read_words(&aggregate_readback, 12), vec![0; 12]);
        let empty = read_plan(&plan_readback);
        assert_eq!(empty.maximum_speed, 0.0);
        assert_eq!(empty.dt, p.frame_duration);

        // Scheduling must stop on remaining == 0 even while the previous
        // step's dt is positive. schedule then sets dt to zero, so subsequent
        // cleanup must also leave both poisoned buffers untouched.
        let final_step = PlanValue {
            dt: 0.1,
            elapsed: 1.0,
            remaining: 0.0,
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        // SAFETY: all earlier work has completed; plan_input is shared storage.
        unsafe { plan_input.write(0, bytemuck::bytes_of(&final_step)) };
        // The empty population above reduced one zero partial into scratch;
        // poison again so this stage sees only what inactive passes write.
        let mut enc = device.create_encoder("flip-clock inactive scheduling and cleanup");
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_a, poison.size);
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_b, poison.size);
        enc.copy_buffer_to_buffer(&plan_input, &clock.plan, 48);
        dispatch(&mut enc, &full, COUNT);
        clock.remove_extreme(&mut enc, &markers, &full, &p);
        copy_scratch(&mut enc);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        assert_poison();
        assert_eq!(read_plan(&plan_readback).dt, 0.0);
        let unchanged = read_words(&markers, COUNT as usize * 8);
        assert_eq!(
            unchanged.as_slice(),
            bytemuck::cast_slice::<FluidParticle, u32>(&particles)
        );

        // The final accepted step still needs the complete reduction and
        // removal, despite having no interval remainder left to schedule.
        let mut enc = device.create_encoder("flip-clock final active cleanup");
        enc.copy_buffer_to_buffer(&plan_input, &clock.plan, 48);
        clock.remove_extreme(&mut enc, &markers, &full, &p);
        enc.copy_buffer_to_buffer(&clock.marker_result, &aggregate_readback, 16);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        assert_eq!(
            f32::from_bits(read_words(&aggregate_readback, 4)[0]),
            1024.0
        );
        let result = read_plan(&plan_readback);
        assert_eq!(result.remaining, 0.0);
        assert_eq!(result.dt, final_step.dt);
        assert_eq!(
            result.marker_limit,
            p.max_frame_steps as f32 * p.cfl * p.cell_size / p.limit_interval
        );
        // SAFETY: the encoder completed and the buffer holds COUNT records.
        let got = unsafe {
            std::slice::from_raw_parts(
                markers.mapped_ptr().unwrap().cast::<FluidParticle>(),
                COUNT as usize,
            )
        };
        for (index, (before, after)) in particles.iter().zip(got).enumerate() {
            let mut want = *before;
            if index == COUNT as usize - 1 {
                want.position_radius[3] = 0.0;
            }
            assert_eq!(bytemuck::bytes_of(after), bytemuck::bytes_of(&want));
        }
    }

    #[test]
    fn gpu_flip_clock_marker_reduction_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 512, 1, 1);
        let markers = device.create_buffer(512 * 32, manifold_gpu::BufferUse::Upload);
        let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let mut particles = [particle(1.0); 512];
        particles[511].velocity = [3.0, 4.0, 0.0];
        particles[2] = particle(1000.0);
        particles[2].position_radius[3] = 0.0;
        unsafe {
            markers.write(0, bytemuck::cast_slice(&particles));
        }
        let p = GpuFlipClockParams {
            frame_duration: 0.1,
            limit_interval: 0.1,
            cfl: 2.0,
            ..params()
        };
        let speed = particles
            .iter()
            .filter(|v| v.position_radius[3] > 0.0)
            .map(|v| {
                v.velocity
                    .iter()
                    .map(|x| f64::from(*x).powi(2))
                    .sum::<f64>()
                    .sqrt()
            })
            .fold(0.0, f64::max);
        for count in [512, 0] {
            let record = slots_record(&device, count);
            let mut enc = device.create_encoder("flip-clock marker proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &record,
                    obstacle_vertices: &empty,
                    obstacle_count: 0,
                    source_vertices: &empty,
                    source_count: 0,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            clock.remove_extreme(&mut enc, &markers, &record, &p);
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            let result = read_plan(&readback);
            let expected_speed = if count == 0 { 0.0 } else { speed };
            assert!((f64::from(result.maximum_speed) - expected_speed).abs() < 1e-5);
            let expected_limit = p.max_frame_steps as f32 * p.cfl * p.cell_size / p.limit_interval;
            assert!((result.marker_limit - expected_limit).abs() < 1e-5);
            assert!(
                (result.dt - expected_dt(&p, expected_speed, CflRestrictions::default())).abs()
                    < 1e-6
            );
            assert_eq!(result.nonfinite, 0);
        }
    }

    #[test]
    fn gpu_flip_clock_marker_histogram_removal_value_proof() {
        // Native removal bins speeds by CFL * cell / the whole frame, even when
        // the scheduler splits it: _removeMarkerParticles is passed
        // _currentFrameDeltaTime. Every case is a CFL split where a step-dt
        // width gives a different limit. Speeds sit mid-bin, because a speed
        // exactly on a bin edge lands in different bins in the f32 kernel and
        // the f64 reference. Nine 11 m/s outliers keep the relative clamp out
        // of play. At 1 s they share the top bin with one 3.25 m/s marker:
        // 300 markers have no removal budget, so the six-step floor removes
        // it; 0.05% of 21,000 is 10.5, so the budget admits the top bin and
        // the limit walks down to (bin + 4) widths, which keeps it. Each case
        // runs as export (limit_interval 0 measures the frame) and as a late
        // live frame four intervals long, measured as one interval: it removes
        // exactly what export removes, where the span's own width would lower
        // the limit and delete ordinary water.
        use manifold_core::Seconds;
        use manifold_physics::stepping::{MarkerSpeedLimitConfig, marker_particle_speed_limit};
        let device = manifold_gpu::testkit::test_device();
        let cases = [(300u32, 0.25, 12.0), (300, 1.0, 3.0), (21_000, 1.0, 3.5)];
        for ((count, duration, limit), span) in cases
            .into_iter()
            .flat_map(|case| [(case, 1.0f32), (case, 4.0)])
        {
            let clock = GpuFlipClock::new(&device, count, 1, 1);
            let markers = device.create_buffer(u64::from(count) * 32, manifold_gpu::BufferUse::Upload);
            let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
            let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
            let mut particles = vec![particle(1.25); count as usize];
            for (i, marker) in particles.iter_mut().enumerate() {
                marker.id = i as u32 + 101;
            }
            for marker in particles.iter_mut().take(9) {
                marker.velocity = [11.0, 0.0, 0.0];
            }
            particles[9].velocity = [3.25, 0.0, 0.0];
            unsafe {
                markers.write(0, bytemuck::cast_slice(&particles));
            }
            let limit_interval = if span == 1.0 { 0.0 } else { duration };
            let p = GpuFlipClockParams {
                frame_duration: duration * span,
                limit_interval,
                ..params()
            };
            let record = slots_record(&device, count);
            let mut enc = device.create_encoder("flip-clock marker histogram proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &record,
                    obstacle_vertices: &empty,
                    obstacle_count: 0,
                    source_vertices: &empty,
                    source_count: 0,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            clock.remove_extreme(&mut enc, &markers, &record, &p);
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            let result = read_plan(&readback);
            assert!((result.maximum_speed - 11.0).abs() < 1e-5);
            let speeds: Vec<f64> = particles.iter().map(|m| f64::from(m.velocity[0])).collect();
            let cpu_limit = |dt: f32| {
                marker_particle_speed_limit(
                    &speeds,
                    Seconds(f64::from(dt)),
                    f64::from(p.cell_size),
                    f64::from(p.cfl),
                    p.max_frame_steps,
                    MarkerSpeedLimitConfig::default(),
                    &mut [0; 6],
                )
                .value as f32
            };
            let case = format!("{count} markers, {duration} s frame, {span}x span");
            assert!(result.dt < p.frame_duration, "{case}: no CFL split");
            if span == 1.0 {
                assert_ne!(cpu_limit(result.dt), limit, "{case}: step-dt width agrees");
            } else {
                assert_ne!(
                    cpu_limit(p.frame_duration),
                    limit,
                    "{case}: span width agrees"
                );
            }
            assert_eq!(cpu_limit(duration), limit, "{case}: CPU");
            assert_eq!(result.marker_limit, limit, "{case}: GPU");
            let got = unsafe {
                std::slice::from_raw_parts(
                    markers.mapped_ptr().unwrap().cast::<FluidParticle>(),
                    count as usize,
                )
            };
            for (before, after) in particles.iter().zip(got) {
                assert_eq!(after.position_radius[3] > 0.0, before.velocity[0] <= limit);
                assert_eq!(before.id, after.id);
            }
        }
    }

    #[test]
    fn gpu_flip_clock_obstacle_acceleration_angular_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 2, 1);
        let none = slots_record(&device, 0);
        let markers = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let obstacles = device.create_buffer(192, manifold_gpu::BufferUse::Upload);
        let sources = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let vertex = GpuFlipBodyVertex {
            position: [1.0, 0.0, 0.0, 1.0],
            velocity: [1.0, 0.0, 0.0, 0.0],
            acceleration: [1.0, 0.0, 0.0, 0.0],
            angular_velocity: [0.0, 0.0, 2.0, 0.0],
            angular_acceleration: [0.0, 0.0, 1.0, 0.0],
            centroid: [0.0; 4],
        };
        let p = GpuFlipClockParams {
            cell_size: 1.0,
            flags: flags::FLUID_PRESENT_OR_GENERATING,
            ..params()
        };
        // Check acceleration, deceleration (initial endpoint wins), and the
        // eligibility bit supplied for out-of-domain noncoupled geometry.
        for (acceleration, eligible, expected_speed) in [
            ([1.0, 0.0, 0.0, 0.0], 1.0, 13.0_f32.sqrt()),
            ([-2.0, -4.0, 0.0, 0.0], 1.0, 5.0_f32.sqrt()),
            ([1.0, 0.0, 0.0, 0.0], 0.0, 0.0),
        ] {
            let mut current = vertex;
            current.acceleration = acceleration;
            current.position[3] = eligible;
            unsafe {
                obstacles.write(0, bytemuck::bytes_of(&current));
            }
            let mut enc = device.create_encoder("flip-clock obstacle proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &none,
                    obstacle_vertices: &obstacles,
                    obstacle_count: 1,
                    source_vertices: &sources,
                    source_count: 0,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            assert!((read_plan(&readback).maximum_speed - expected_speed).abs() < 1e-5);
        }
    }

    #[test]
    fn gpu_flip_clock_coupled_reaction_shortens_next_cfl_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let none = slots_record(&device, 0);
        let markers = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let obstacles = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let sources = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let rows = device.create_buffer(size_of::<LiquidBody>() as u64, manifold_gpu::BufferUse::Upload);
        let reaction = device.create_buffer(8 * size_of::<f32>() as u64, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let vertex = GpuFlipBodyVertex {
            position: [1.0, 0.0, 0.0, 1.0],
            velocity: [0.0, 0.0, 0.0, 1.0],
            acceleration: [0.0; 4],
            angular_velocity: [0.0; 4],
            angular_acceleration: [0.0; 4],
            centroid: [0.0; 4],
        };
        let body = LiquidBody {
            position_inv_mass: [0.0, 0.0, 0.0, 1.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [0.0; 4],
            angular_velocity: [0.0; 4],
            inv_inertia_x: [1.0, 0.0, 0.0, 0.0],
            inv_inertia_y: [0.0, 1.0, 0.0, 0.0],
            inv_inertia_z: [0.0, 0.0, 1.0, 0.0],
            accel_shape: [0.0; 4],
        };
        unsafe {
            obstacles.write(0, bytemuck::bytes_of(&vertex));
            rows.write(0, bytemuck::bytes_of(&body));
            reaction.write(0, bytemuck::cast_slice(&[0.0_f32; 8]));
        }
        let p = GpuFlipClockParams {
            frame_duration: 0.1,
            cell_size: 0.1,
            cfl: 5.0,
            flags: flags::FLUID_PRESENT_OR_GENERATING,
            ..params()
        };
        let dispatch = |label: &str| {
            let mut enc = device.create_encoder(label);
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &none,
                    obstacle_vertices: &obstacles,
                    obstacle_count: 1,
                    source_vertices: &sources,
                    source_count: 0,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &rows,
                    body_rows_offset: 0,
                    body_reaction: &reaction,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            read_plan(&readback)
        };
        let without_reaction = dispatch("flip-clock coupled body zero reaction proof");
        unsafe {
            reaction.write(
                0,
                bytemuck::cast_slice(&[10.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            );
        }
        let with_reaction = dispatch("flip-clock coupled body reaction proof");
        assert_eq!(without_reaction.maximum_speed, 0.0);
        assert_eq!(without_reaction.dt, p.frame_duration);
        assert_eq!(with_reaction.maximum_speed, 10.0);
        assert!(with_reaction.dt < without_reaction.dt);
        assert_eq!(
            with_reaction.dt,
            expected_dt(&p, 10.0, CflRestrictions::default())
        );
    }

    #[test]
    fn gpu_flip_clock_later_intervals_skip_source_reductions_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        // More than one workgroup also exercises the partial reduction that
        // must disappear with the unused source scan.
        const SOURCES: u32 = 129;
        let clock = GpuFlipClock::new(&device, 1, 1, SOURCES);
        let zeros = device.create_buffer(128, manifold_gpu::BufferUse::Upload);
        zeros.zero_fill();
        let sources =
            device.create_buffer(u64::from(SOURCES) * size_of::<GpuFlipBodyVertex>() as u64, manifold_gpu::BufferUse::Upload);
        let source = GpuFlipBodyVertex {
            position: [0.0, 0.0, 0.0, 1.0],
            velocity: [3.0, 0.0, 0.0, 0.0],
            ..GpuFlipBodyVertex::default()
        };
        unsafe { sources.write(0, bytemuck::cast_slice(&[source; SOURCES as usize])) };
        let inputs = |source_count| GpuFlipClockInputs {
            marker_particles: &zeros,
            marker_count: &zeros,
            obstacle_vertices: &zeros,
            obstacle_count: 0,
            source_vertices: &sources,
            source_count,
            live_hits: &zeros,
            live_hit_count: 0,
            event_impulses: &zeros,
            impulse_stride: 0,
            impulse_nodes: [2; 3],
            impulse_origin: [0.0; 3],
            impulse_spacing: 1.0,
            body_rows: &zeros,
            body_rows_offset: 0,
            body_reaction: &zeros,
        };
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let initial = GpuFlipClockParams {
            flags: flags::FIRST_SUBSTEP,
            ..params()
        };
        let mut enc = device.create_encoder("flip-clock initial source proof");
        clock.begin_frame(&mut enc, &initial);
        let plan = clock.dispatch(&mut enc, inputs(SOURCES), &initial);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        assert_eq!(read_plan(&readback).maximum_speed, 3.0);

        let untouched = vec![0x12345678u32; (clock.scratch_a.size / 4) as usize];
        let seed = device.create_buffer(clock.scratch_a.size, manifold_gpu::BufferUse::Upload);
        let scratch = device.create_buffer(clock.scratch_a.size, manifold_gpu::BufferUse::Readback);
        unsafe { seed.write(0, bytemuck::cast_slice(&untouched)) };
        let later = params();
        let mut enc = device.create_encoder("flip-clock later source work proof");
        enc.copy_buffer_to_buffer(&seed, &clock.scratch_a, seed.size);
        clock.begin_frame(&mut enc, &later);
        let plan = clock.dispatch(&mut enc, inputs(SOURCES), &later);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.copy_buffer_to_buffer(&clock.scratch_a, &scratch, scratch.size);
        enc.commit_and_wait().expect("GPU work completes");
        let result = read_plan(&readback);
        assert_eq!(
            (result.maximum_speed, result.dt, result.nonfinite),
            (0.0, later.frame_duration, 0)
        );
        let actual = unsafe {
            std::slice::from_raw_parts(scratch.mapped_ptr().unwrap().cast::<u32>(), untouched.len())
        };
        assert_eq!(
            actual, untouched,
            "later intervals must not scan sources or reduce their partials"
        );

        let mut enc = device.create_encoder("flip-clock absent source reference");
        clock.begin_frame(&mut enc, &later);
        let plan = clock.dispatch(&mut enc, inputs(0), &later);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        assert_eq!(
            bytemuck::bytes_of(&result),
            bytemuck::bytes_of(&read_plan(&readback))
        );
    }

    #[test]
    fn gpu_flip_clock_source_prediction_and_restrictions_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let one = slots_record(&device, 1);
        let markers = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let bodies = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let source = GpuFlipBodyVertex {
            position: [0.0, 0.0, 0.0, 1.0],
            velocity: [3.0, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        unsafe {
            markers.write(0, bytemuck::bytes_of(&particle(10.0)));
            bodies.write(0, bytemuck::bytes_of(&source));
        }
        for restriction in [0, flags::SURFACE_TENSION, flags::COLOR_MIXING] {
            let p = GpuFlipClockParams {
                flags: flags::FIRST_SUBSTEP | flags::FLUID_PRESENT_OR_GENERATING | restriction,
                constant_force: [-2.0, 0.0, 0.0, 0.0],
                surface_condition: 0.5,
                surface_constant: 2.0,
                color_mixing_rate: 37.5,
                ..params()
            };
            let limits = CflRestrictions {
                surface_tension: (restriction == flags::SURFACE_TENSION).then_some((0.5, 2.0)),
                color_mixing_rate: (restriction == flags::COLOR_MIXING).then_some(37.5),
            };
            let mut enc = device.create_encoder("flip-clock source proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &one,
                    obstacle_vertices: &bodies,
                    obstacle_count: 0,
                    source_vertices: &bodies,
                    source_count: 1,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            let result = read_plan(&readback);
            assert_eq!(result.maximum_speed, 5.0); // |source| + |force| * frame
            assert!((result.dt - expected_dt(&p, 5.0, limits)).abs() < 1e-6);
            let mut enc = device.create_encoder("flip-clock current marker proof");
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &one,
                    obstacle_vertices: &bodies,
                    obstacle_count: 0,
                    source_vertices: &bodies,
                    source_count: 1,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait().expect("GPU work completes");
            assert_eq!(read_plan(&readback).maximum_speed, 10.0);
        }
    }

    /// A live tick that encoded slots for two of six steps: the second takes
    /// all the frame's remaining time and reports the cap, while the marker
    /// speed limit still measures against all six.
    #[test]
    fn gpu_flip_clock_encoded_steps_end_the_frame_early() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let one = slots_record(&device, 1);
        let markers = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        unsafe {
            markers.write(0, bytemuck::bytes_of(&particle(1000.0)));
        }
        let run = |encoded_steps: u32| {
            let p = GpuFlipClockParams {
                encoded_steps,
                ..params()
            };
            let plans = [
                device.create_buffer(48, manifold_gpu::BufferUse::Upload),
                device.create_buffer(48, manifold_gpu::BufferUse::Upload),
            ];
            let mut enc = device.create_encoder("flip-clock encoded steps proof");
            clock.begin_frame(&mut enc, &p);
            for out in &plans {
                let plan = clock.dispatch(
                    &mut enc,
                    GpuFlipClockInputs {
                        marker_particles: &markers,
                        marker_count: &one,
                        obstacle_vertices: &empty,
                        obstacle_count: 0,
                        source_vertices: &empty,
                        source_count: 0,
                        live_hits: &markers,
                        live_hit_count: 0,
                        event_impulses: &markers,
                        impulse_stride: 0,
                        impulse_nodes: [2, 2, 2],
                        impulse_origin: [0.0; 3],
                        impulse_spacing: 1.0,
                        body_rows: &markers,
                        body_rows_offset: 0,
                        body_reaction: &markers,
                    },
                    &p,
                );
                enc.copy_buffer_to_buffer(plan.buffer(), out, 48);
            }
            enc.commit_and_wait().expect("GPU work completes");
            (p, read_plan(&plans[0]), read_plan(&plans[1]))
        };
        let (p, first, second) = run(2);
        assert_eq!(first.cap_hit, 0);
        assert!(first.remaining > 0.0);
        assert_eq!(second.dt, first.remaining);
        assert_eq!(second.elapsed, p.frame_duration);
        assert_eq!(second.remaining, 0.0);
        assert_eq!(second.step_index, 2);
        assert_eq!(second.cap_hit, 1);
        let (_, all_first, all_second) = run(0);
        assert_eq!(all_first.dt, first.dt, "the first step keeps its CFL step");
        assert!(all_second.remaining > 0.0, "with every slot the frame goes on");
        assert_eq!(all_first.marker_limit, first.marker_limit);
    }

    #[test]
    fn gpu_flip_clock_cap_and_nonfinite_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let one = slots_record(&device, 1);
        let markers = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let before_cap = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let p = params();
        unsafe {
            markers.write(0, bytemuck::bytes_of(&particle(1000.0)));
        }
        let mut enc = device.create_encoder("flip-clock cap proof");
        clock.begin_frame(&mut enc, &p);
        for step in 0..p.max_frame_steps {
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: &one,
                    obstacle_vertices: &empty,
                    obstacle_count: 0,
                    source_vertices: &empty,
                    source_count: 0,
                    live_hits: &markers,
                    live_hit_count: 0,
                    event_impulses: &markers,
                    impulse_stride: 0,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: &markers,
                    body_rows_offset: 0,
                    body_reaction: &markers,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            if step + 2 == p.max_frame_steps {
                enc.copy_buffer_to_buffer(plan.buffer(), &before_cap, 48);
            }
        }
        enc.commit_and_wait().expect("GPU work completes");
        let result = read_plan(&readback);
        let before = read_plan(&before_cap);
        assert_eq!(before.step_index, 5);
        assert_eq!(before.cap_hit, 0);
        assert!(before.remaining > 0.99 && before.remaining < p.frame_duration);
        // The cap consumes the actual GPU cursor exactly. A host-derived
        // CFL quotient at an integer ceil boundary can choose the adjacent
        // count after Metal reciprocal lowering; it is not this invariant.
        assert_eq!(result.dt, before.remaining);
        assert_eq!(result.elapsed, p.frame_duration);
        assert_eq!(result.remaining, 0.0);
        assert_eq!(result.step_index, 6);
        assert_eq!(result.cap_hit, 1);
        assert_eq!(result.nonfinite, 0);
        unsafe {
            markers.write(0, bytemuck::bytes_of(&particle(f32::NAN)));
        }
        let mut enc = device.create_encoder("flip-clock nonfinite proof");
        clock.begin_frame(&mut enc, &p);
        let plan = clock.dispatch(
            &mut enc,
            GpuFlipClockInputs {
                marker_particles: &markers,
                marker_count: &one,
                obstacle_vertices: &empty,
                obstacle_count: 0,
                source_vertices: &empty,
                source_count: 0,
                live_hits: &markers,
                live_hit_count: 0,
                event_impulses: &markers,
                impulse_stride: 0,
                impulse_nodes: [2, 2, 2],
                impulse_origin: [0.0; 3],
                impulse_spacing: 1.0,
                body_rows: &markers,
                body_rows_offset: 0,
                body_reaction: &markers,
            },
            &p,
        );
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        let result = read_plan(&readback);
        assert_eq!(result.dt, 1.0);
        assert_eq!(result.elapsed, 1.0);
        assert_eq!(result.remaining, 0.0);
        assert_eq!(result.nonfinite, 1);
    }

    #[test]
    fn gpu_flip_clock_event_boundary_predicts_impulse_once_and_reaches_cap() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let one = slots_record(&device, 1);
        let before_hit = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let after_hit = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let hits = device.create_buffer(32, manifold_gpu::BufferUse::Upload);
        let impulses = device.create_buffer(32 * 4, manifold_gpu::BufferUse::Upload);
        let readbacks: Vec<_> = (0..7).map(|_| device.create_buffer(48, manifold_gpu::BufferUse::Upload)).collect();
        // Another interval's earlier event must neither split this interval
        // nor contribute to its predicted impulse velocity.
        let hit = [
            0.01_f32,
            0.0,
            f32::from_bits(6),
            0.0,
            0.02_f32,
            0.0,
            f32::from_bits(7),
            0.0,
        ];
        unsafe {
            hits.write(0, bytemuck::cast_slice(&hit));
            let mut lattice = [0.0_f32; 32];
            for node in lattice.chunks_exact_mut(4) {
                node[0] = 100.0;
            }
            impulses.write(0, bytemuck::cast_slice(&lattice));
            before_hit.write(0, bytemuck::bytes_of(&particle(0.0)));
            after_hit.write(0, bytemuck::bytes_of(&particle(100.0)));
        }
        let p = GpuFlipClockParams {
            frame_duration: 0.1,
            min_frame_steps: 1,
            max_frame_steps: 6,
            interval_sequence: 7,
            ..params()
        };
        let mut enc = device.create_encoder("flip-clock live event proof");
        clock.begin_frame(&mut enc, &p);
        for (index, readback) in readbacks.iter().enumerate() {
            // The event segment predicts the hit impulse from the zero-speed
            // pre-hit population. Subsequent segments receive the actual
            // post-hit marker population, so a prediction cannot be applied
            // twice.
            let markers = if index < 2 { &before_hit } else { &after_hit };
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: markers,
                    marker_count: &one,
                    obstacle_vertices: &empty,
                    obstacle_count: 0,
                    source_vertices: &empty,
                    source_count: 0,
                    live_hits: &hits,
                    live_hit_count: 2,
                    event_impulses: &impulses,
                    impulse_stride: 32,
                    impulse_nodes: [2, 2, 2],
                    impulse_origin: [0.0; 3],
                    impulse_spacing: 1.0,
                    body_rows: markers,
                    body_rows_offset: 0,
                    body_reaction: markers,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), readback, 48);
        }
        enc.commit_and_wait().expect("GPU work completes");
        let plans: Vec<_> = readbacks.iter().map(read_plan).collect();
        assert_eq!(plans[0].elapsed, 0.02);
        assert_eq!(plans[0].step_index, 0);
        assert_eq!(plans[0].cap_hit, 0);
        assert_eq!(plans[1].maximum_speed, 100.0);
        let event_dt = expected_dt(&p, 100.0, CflRestrictions::default());
        // Metal may lower division to reciprocal multiplication. This
        // non-binary fraction can differ from host division by one f32 ulp.
        assert!(plans[1].dt.to_bits().abs_diff(event_dt.to_bits()) <= 1);
        assert!((plans[1].elapsed - (0.02 + event_dt)).abs() < 1.0e-6);
        assert_eq!(plans[1].step_index, 1);
        assert_ne!(plans[1].pad & 0x8000_0000, 0);
        assert_eq!(plans[2].pad, 0);
        assert_eq!(plans[6].elapsed, 0.1);
        assert_eq!(plans[6].remaining, 0.0);
        assert_eq!(plans[6].step_index, 6);
        assert_eq!(plans[6].cap_hit, 1);
        assert_eq!(
            plans
                .iter()
                .filter(|plan| plan.pad & 0x8000_0000 != 0)
                .count(),
            1
        );
    }

    /// The triple `marker_args` wrote for `record` under `plan`.
    fn marker_triple_for(
        device: &GpuDevice,
        clock: &GpuFlipClock,
        plan: &PlanValue,
        record: &GpuBuffer,
        phase: ReductionPhase,
    ) -> Vec<u32> {
        let plan_seed = device.create_buffer(48, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(16, manifold_gpu::BufferUse::Readback);
        // SAFETY: fresh shared buffer, no GPU work in flight.
        unsafe { plan_seed.write(0, bytemuck::bytes_of(plan)) };
        let mut enc = device.create_encoder("flip-clock marker args proof");
        enc.copy_buffer_to_buffer(&plan_seed, &clock.plan, 48);
        clock.marker_args(&mut enc, record, phase, false);
        enc.copy_buffer_to_buffer(&clock.marker_triple, &readback, 16);
        enc.commit_and_wait().expect("GPU work completes");
        read_words(&readback, 4)
    }

    #[test]
    fn gpu_flip_clock_marker_triple_follows_the_count_and_the_phase() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 512, 1, 1);
        let live = PlanValue {
            dt: 0.1,
            remaining: 0.5,
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        // 64-thread groups over n, at least one, n capped at the capacity.
        for (n, groups, covered) in [
            (0, 1, 0),
            (1, 1, 1),
            (63, 1, 63),
            (64, 1, 64),
            (65, 2, 65),
            (300, 5, 300),
            (512, 8, 512),
            (513, 8, 512),
            (100_000, 8, 512),
        ] {
            let record = slots_record(&device, n);
            for phase in [ReductionPhase::Scheduling, ReductionPhase::Cleanup] {
                assert_eq!(
                    marker_triple_for(&device, &clock, &live, &record, phase),
                    [groups, 1, 1, covered],
                    "n {n}"
                );
            }
        }
        // Scheduling needs time left; cleanup needs a step taken. A plan with
        // neither launches nothing, whatever the population; offline always runs.
        let record = slots_record(&device, 300);
        let no_time = PlanValue {
            dt: 0.1,
            remaining: 0.0,
            ..live
        };
        let no_step = PlanValue {
            dt: 0.0,
            remaining: 0.5,
            ..live
        };
        let idle = PlanValue {
            dt: 0.0,
            remaining: 0.0,
            ..live
        };
        let offline = PlanValue {
            live_mode: 0,
            ..idle
        };
        for (plan, scheduling, cleanup, label) in [
            (no_time, 0, 5, "no time left"),
            (no_step, 5, 0, "no step taken"),
            (idle, 0, 0, "idle"),
            (offline, 5, 5, "offline"),
        ] {
            for (phase, groups) in [
                (ReductionPhase::Scheduling, scheduling),
                (ReductionPhase::Cleanup, cleanup),
            ] {
                let triple = marker_triple_for(&device, &clock, &plan, &record, phase);
                assert_eq!(triple, [groups, 1, 1, 300], "{label}");
            }
        }
    }

    /// The top speed the clock plans from one frame over `markers`, and the
    /// plan's bytes.
    fn planned_speed(
        device: &GpuDevice,
        clock: &GpuFlipClock,
        markers: &GpuBuffer,
        record: &GpuBuffer,
    ) -> PlanValue {
        let empty = device.create_buffer(96, manifold_gpu::BufferUse::Upload);
        let readback = device.create_buffer(48, manifold_gpu::BufferUse::Readback);
        let p = params();
        let mut enc = device.create_encoder("flip-clock slots in use proof");
        clock.begin_frame(&mut enc, &p);
        let plan = clock.dispatch(
            &mut enc,
            GpuFlipClockInputs {
                marker_particles: markers,
                marker_count: record,
                obstacle_vertices: &empty,
                obstacle_count: 0,
                source_vertices: &empty,
                source_count: 0,
                live_hits: &empty,
                live_hit_count: 0,
                event_impulses: &empty,
                impulse_stride: 0,
                impulse_nodes: [2, 2, 2],
                impulse_origin: [0.0; 3],
                impulse_spacing: 1.0,
                body_rows: &empty,
                body_rows_offset: 0,
                body_reaction: &empty,
            },
            &p,
        );
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait().expect("GPU work completes");
        read_plan(&readback)
    }

    #[test]
    fn gpu_flip_clock_top_speed_ignores_slots_past_the_count() {
        // 70,000 slots reduce in three levels (1,094 and 18 partials), and
        // the counts below end in the middle of a group at each level.
        const CAPACITY: usize = 70_000;
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, CAPACITY as u32, 1, 1);
        for n in [1usize, 300, 4_097, 65_000, CAPACITY] {
            let mut particles = vec![particle(1.0); CAPACITY];
            particles[n - 1] = particle(3.0);
            let clean = device.create_buffer(CAPACITY as u64 * 32, manifold_gpu::BufferUse::Upload);
            // A faster decoy and a non-finite one past the count.
            let mut decoys = particles.clone();
            for decoy in decoys.iter_mut().skip(n) {
                *decoy = particle(1000.0);
            }
            if n < CAPACITY {
                decoys[n].velocity[0] = f32::NAN;
            }
            let decoyed = device.create_buffer(CAPACITY as u64 * 32, manifold_gpu::BufferUse::Upload);
            for particle in particles.iter_mut().skip(n) {
                *particle = FluidParticle::zeroed();
            }
            // SAFETY: fresh shared buffers, no GPU work in flight.
            unsafe {
                clean.write(0, bytemuck::cast_slice(&particles));
                decoyed.write(0, bytemuck::cast_slice(&decoys));
            }
            let record = slots_record(&device, n as u32);
            let want = planned_speed(&device, &clock, &clean, &record);
            let got = planned_speed(&device, &clock, &decoyed, &record);
            assert_eq!(want.maximum_speed, 3.0, "n {n}");
            assert_eq!(want.nonfinite, 0, "n {n}");
            assert_eq!(
                bytemuck::bytes_of(&got),
                bytemuck::bytes_of(&want),
                "n {n}: a decoy past the count changed the plan"
            );
            if n < CAPACITY {
                // The decoys are seen once the count covers them.
                let all = slots_record(&device, CAPACITY as u32);
                assert_eq!(
                    planned_speed(&device, &clock, &decoyed, &all).nonfinite,
                    1,
                    "n {n}"
                );
            }
        }
    }

    #[test]
    fn gpu_flip_clock_removal_leaves_particles_past_the_count_alone() {
        const CAPACITY: usize = 512;
        const COUNT: usize = 300;
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, CAPACITY as u32, 1, 1);
        let record = slots_record(&device, COUNT as u32);
        let markers = device.create_buffer(CAPACITY as u64 * 32, manifold_gpu::BufferUse::Upload);
        let mut particles = vec![particle(1.0); CAPACITY];
        // Over the limit inside the count and at every slot past it.
        particles[COUNT - 1] = particle(1024.0);
        for decoy in particles.iter_mut().skip(COUNT) {
            *decoy = particle(1024.0);
        }
        // SAFETY: fresh shared buffer, no GPU work in flight.
        unsafe { markers.write(0, bytemuck::cast_slice(&particles)) };
        let plan_seed = device.create_buffer(48, manifold_gpu::BufferUse::Upload);
        let step = PlanValue {
            dt: 0.1,
            elapsed: 1.0,
            remaining: 0.0,
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        // SAFETY: fresh shared buffer, no GPU work in flight.
        unsafe { plan_seed.write(0, bytemuck::bytes_of(&step)) };
        let mut enc = device.create_encoder("flip-clock removal past the count");
        enc.copy_buffer_to_buffer(&plan_seed, &clock.plan, 48);
        clock.remove_extreme(&mut enc, &markers, &record, &params());
        enc.commit_and_wait().expect("GPU work completes");
        // SAFETY: the encoder completed and the buffer holds CAPACITY records.
        let got = unsafe {
            std::slice::from_raw_parts(
                markers.mapped_ptr().unwrap().cast::<FluidParticle>(),
                CAPACITY,
            )
        };
        for (index, (before, after)) in particles.iter().zip(got).enumerate() {
            let mut want = *before;
            if index == COUNT - 1 {
                want.position_radius[3] = 0.0;
            }
            assert_eq!(
                bytemuck::bytes_of(after),
                bytemuck::bytes_of(&want),
                "slot {index}"
            );
        }
    }
}
