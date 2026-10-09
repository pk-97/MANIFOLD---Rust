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

const SHADER: &str = include_str!("shaders/gpu_flip_clock.wgsl");
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
    #[cfg(all(test, feature = "gpu-proofs"))]
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
    pub _pad_prediction: f32,
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

/// A body vertex used by both obstacle and initial-source prediction.
/// `velocity` is the current linear velocity (for a source it already
/// includes the source's fluid velocity). `velocity.w` is zero for prescribed
/// geometry and is a coupled body-row index plus one for dynamic hulls.
/// `position.w` is an eligibility bit; noncoupled vertices outside the liquid
/// domain have zero there. Coupled hull vertices stay eligible even when
/// outside, as in the reference engine.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuFlipBodyVertex {
    pub position: [f32; 4],
    pub velocity: [f32; 4],
    pub acceleration: [f32; 4],
    pub angular_velocity: [f32; 4],
    pub angular_acceleration: [f32; 4],
    pub centroid: [f32; 4],
}

/// Inputs to one scheduling dispatch.  Buffers are persistent solver-owned
/// GPU buffers.  Counts may be any value that fits the corresponding buffer;
/// there is no quality or population cap in this helper.
pub struct GpuFlipClockInputs<'a> {
    /// `FluidParticle` records (32-byte packed layout).  A zero radius is an
    /// unused slot and is excluded by the reduction shader.
    pub marker_particles: &'a GpuBuffer,
    pub marker_count: u32,
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
    marker_reduce: GpuComputePipeline,
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
        2 * partial_capacity * 16 + 16 * 3 + u64::from(HISTOGRAM_BINS) * 4 + 16 + 48
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
            marker_reduce: device.create_compute_pipeline(
                SHADER,
                "reduce_marker",
                "flip-clock-marker",
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
            scratch_a: device.create_buffer(scratch_bytes),
            scratch_b: device.create_buffer(scratch_bytes),
            marker_result: device.create_buffer(16),
            obstacle_result: device.create_buffer(16),
            source_result: device.create_buffer(16),
            marker_histogram: device.create_buffer(u64::from(HISTOGRAM_BINS) * 4),
            marker_outliers: device.create_buffer(16),
            plan: device.create_buffer(48),
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
        // Zero-count populations must not reuse a previous frame's aggregate.
        // This is a GPU blit, not a CPU readback or allocation.
        encoder.clear_buffer(&self.marker_result);
        encoder.clear_buffer(&self.obstacle_result);
        encoder.clear_buffer(&self.source_result);
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
        if inputs.marker_count > 0 {
            let marker = self.reduce_marker(
                encoder,
                inputs.marker_particles,
                inputs.marker_count,
                inputs.live_hits,
                inputs.live_hit_count,
                inputs.event_impulses,
                &event_params,
                params,
                ReductionPhase::Scheduling,
            );
            encoder.copy_buffer_to_buffer(marker, &self.marker_result, 16);
            // The reduction result is consumed by the classification pass to
            // apply the native relative-outlier thresholds. Keep this explicit
            // because the copy and following dispatch target different resources.
            encoder.compute_memory_barrier_buffers();
        }
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

    fn classify_marker(
        &self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        count: u32,
        params: &GpuFlipClockParams,
    ) {
        let reduce_params = reduce_bytes(count, 0, ReductionPhase::Cleanup);
        encoder.dispatch_compute(
            &self.marker_classify,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: &reduce_params,
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
            ],
            [count.div_ceil(WORKGROUP).max(1), 1, 1],
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
    pub(crate) fn remove_extreme(
        &self,
        encoder: &mut GpuEncoder,
        particles: &GpuBuffer,
        count: u32,
        params: &GpuFlipClockParams,
    ) {
        let empty_hits = self.marker_histogram.clone();
        let empty_params = EventFieldParams {
            origin_spacing: [0.0, 0.0, 0.0, 1.0],
            nodes_stride: [2, 2, 2, 0],
        };
        if count > 0 {
            let marker = self.reduce_marker(
                encoder,
                particles,
                count,
                &empty_hits,
                0,
                &empty_hits,
                &empty_params,
                params,
                ReductionPhase::Cleanup,
            );
            encoder.copy_buffer_to_buffer(marker, &self.marker_result, 16);
        } else {
            encoder.clear_buffer(&self.marker_result);
        }
        encoder.compute_memory_barrier_buffers();
        encoder.clear_buffer(&self.marker_histogram);
        encoder.clear_buffer(&self.marker_outliers);
        self.classify_marker(encoder, particles, count, params);
        self.finalize_marker_limit(encoder, params);
        encoder.compute_memory_barrier_buffers();
        let reduce_params = reduce_bytes(count, 0, ReductionPhase::Cleanup);
        encoder.dispatch_compute(
            &self.marker_remove,
            &[
                GpuBinding::Buffer {
                    binding: 25,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: &reduce_params,
                },
                GpuBinding::Buffer {
                    binding: 24,
                    buffer: &self.plan,
                    offset: 0,
                },
            ],
            [count.div_ceil(WORKGROUP).max(1), 1, 1],
            "flip-clock-marker-remove",
        );
    }

    fn reduce_marker<'a>(
        &'a self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        count: u32,
        live_hits: &GpuBuffer,
        live_hit_count: u32,
        event_impulses: &GpuBuffer,
        event_params: &EventFieldParams,
        clock_params: &GpuFlipClockParams,
        phase: ReductionPhase,
    ) -> &'a GpuBuffer {
        debug_assert!(count > 0, "empty marker populations retain the cleared aggregate");
        let mut n = count;
        let mut pass = 0u32;
        let reduce_data = reduce_bytes(n, 0, phase);
        let hit_params = [live_hit_count, 0, 0, 0];
        encoder.dispatch_compute(
            &self.marker_reduce,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.scratch_a,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: &reduce_data,
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
            ],
            [n.div_ceil(WORKGROUP), 1, 1],
            "flip-clock-marker-reduce",
        );
        encoder.compute_memory_barrier_buffers();
        n = n.div_ceil(WORKGROUP);
        while n > 1 {
            let (src, dst) = if pass.is_multiple_of(2) {
                (&self.scratch_a, &self.scratch_b)
            } else {
                (&self.scratch_b, &self.scratch_a)
            };
            let reduce_data = reduce_bytes(n, 0, phase);
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
                    GpuBinding::Buffer { binding: 24, buffer: &self.plan, offset: 0 },
                ],
                [n.div_ceil(WORKGROUP), 1, 1],
                "flip-clock-marker-partial",
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
        debug_assert!(count > 0, "empty body populations retain the cleared aggregate");
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
                GpuBinding::Buffer { binding: 24, buffer: &self.plan, offset: 0 },
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
                    GpuBinding::Buffer { binding: 24, buffer: &self.plan, offset: 0 },
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
        limit = limit.min(condition as f32 * (p.cell_size * p.cell_size * p.cell_size).sqrt()
            * (1.0 / (constant as f32 + 1e-6)).sqrt());
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
        use manifold_physics::stepping::{CflRestrictions, LiveStepSchedule};
        use manifold_physics::Seconds;
        let p = GpuFlipClockParams {
            frame_duration: 0.1, cell_size: 0.1, cfl: 5.0,
            surface_condition: 0.0, surface_constant: 0.0, color_mixing_rate: 0.0,
            _pad_prediction: 0.0, limit_interval: 0.1, min_frame_steps: 1,
            max_frame_steps: 6, flags: 0, interval_sequence: 0, constant_force: [0.0; 4],
        };
        // 0.5 / (10 + epsilon) is below 0.05: ceil requires three
        // subdivisions, not the two in the old reaction proof.
        assert_eq!(expected_dt(&p, 10.0, CflRestrictions::default()), p.frame_duration / 3.0);
        for speed in [0.0, 5.0, 10.0, 100.0, 1000.0] {
            let duration = expected_dt(&p, speed, CflRestrictions::default());
            let mut schedule = LiveStepSchedule::new(Seconds::ZERO,
                Seconds(f64::from(p.frame_duration)), 1, 6).value;
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
    use crate::particles::FluidParticle;
    use crate::water::liquid::bodies::LiquidBody;
    use bytemuck::Zeroable;
    use manifold_physics::stepping::CflRestrictions;
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
            _pad_prediction: 0.0,
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

    fn original_marker_clock(device: &GpuDevice, capacity: u32) -> GpuFlipClock {
        // Frozen global-atomic classifier: independent of the production
        // workgroup implementation, with identical per-particle f32 math.
        const ORIGINAL: &str = r#"@compute @workgroup_size(64)
fn classify_marker(@builtin(global_invocation_id) gid: vec3<u32>) {
    if marker_clock_plan[0].live_mode == 0u || marker_clock_plan[0].step_dt <= 0.0 {
        return;
    }
    let idx = gid.x;
    if idx >= marker_reduce_params.count {
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
        let start = SHADER.find("fn classify_marker_lane(").expect("classifier lane helper");
        let end = SHADER[start..].find("\n// The frame the marker speed limit").expect("classifier end") + start;
        let mut shader = SHADER.to_owned();
        shader.replace_range(start..end, ORIGINAL);
        let mut clock = GpuFlipClock::new(device, capacity, 1, 1);
        clock.marker_classify = device.create_compute_pipeline(
            &shader, "classify_marker", "marker-classify-original-proof",
        );
        clock
    }

    // Setup and readback use separate completed command buffers: total_ms
    // below measures only the existing classifier dispatch and its barrier.
    fn classify_probe(
        device: &GpuDevice,
        clock: &GpuFlipClock,
        input: (&GpuBuffer, u32),
        p: &GpuFlipClockParams,
        plan: &PlanValue,
        seed: Option<&[u32; 68]>,
        buffers: &[GpuBuffer; 4],
    ) -> (Vec<u32>, f64) {
        let (markers, count) = input;
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
        enc.commit_and_wait_completed();
        let mut enc = device.create_encoder("marker-classify-proof-dispatch");
        clock.classify_marker(&mut enc, markers, count, p);
        let timing = enc.commit_and_wait_profiled(device);
        let mut enc = device.create_encoder("marker-classify-proof-readback");
        enc.copy_buffer_to_buffer(&clock.marker_histogram, histogram, 256);
        enc.copy_buffer_to_buffer(&clock.marker_outliers, outliers, 16);
        enc.commit_and_wait_completed();
        let mut words = read_words(histogram, 64);
        words.extend(read_words(outliers, 4));
        (words, timing.total_ms)
    }

    fn classify_buffers(device: &GpuDevice) -> [GpuBuffer; 4] {
        [48, 16, 256, 16].map(|bytes| device.create_buffer_shared(bytes))
    }

    #[test]
    fn gpu_flip_clock_workgroup_marker_histogram_exact_proof() {
        let device = manifold_gpu::testkit::test_device();
        let original = original_marker_clock(&device, 257);
        let candidate = GpuFlipClock::new(&device, 257, 1, 1);
        let markers = device.create_buffer_shared(257 * 32);
        let buffers = classify_buffers(&device);
        let active = PlanValue { dt: 0.25, live_mode: 1, ..PlanValue::zeroed() };
        // Include adjacent f32 words at bin and both relative-outlier edges.
        // Exact parity is against the production GPU math, including sqrt.
        let edges = [0.0, f32::from_bits(0.5f32.to_bits() - 1), 0.5,
            f32::from_bits(0.5f32.to_bits() + 1), 31.75, 32.0,
            f32::from_bits((0.90f32 * 32.0).to_bits() - 1), 0.90 * 32.0,
            f32::from_bits((0.90f32 * 32.0).to_bits() + 1),
            f32::from_bits((0.99999f32 * 32.0).to_bits() - 1), 0.99999 * 32.0,
            f32::from_bits((0.99999f32 * 32.0).to_bits() + 1)];
        for count in [0, 1, 63, 64, 65, 257] {
            for max_steps in [1, 6, 64] {
                let p = GpuFlipClockParams { max_frame_steps: max_steps, ..params() };
                for mixed in [false, true] {
                    let mut particles = vec![particle(0.25); count as usize];
                    if mixed {
                        for (i, marker) in particles.iter_mut().enumerate() {
                            *marker = particle(if i < edges.len() { edges[i] } else { (i % 64) as f32 * 0.5 + 0.25 });
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
                    if let Some(last) = particles.last_mut() { *last = particle(32.0); }
                    unsafe { markers.write(0, bytemuck::cast_slice(&particles)); }
                    let mut previous = None;
                    for repeat in 0..2 {
                        let (old, _) = classify_probe(&device, &original, (&markers, count), &p, &active, None, &buffers);
                        let (new, _) = classify_probe(&device, &candidate, (&markers, count), &p, &active, None, &buffers);
                        assert_eq!(old, new, "count {count}, max_steps {max_steps}, mixed {mixed}, repeat {repeat}");
                        assert_eq!(&new[66..], &[0, 0], "unused outlier words stay clear");
                        if let Some(previous) = &previous { assert_eq!(&new, previous, "cleared repeat"); }
                        previous = Some(new);
                    }
                }
            }
        }
        let seed = std::array::from_fn(|i| 0x1357_0000u32 + i as u32);
        for inactive in [
            PlanValue { live_mode: 0, ..active },
            PlanValue { dt: 0.0, ..active },
        ] {
            for clock in [&original, &candidate] {
                let (words, _) = classify_probe(&device, clock, (&markers, 65), &params(), &inactive, Some(&seed), &buffers);
                assert_eq!(words, seed, "inactive classifier preserves all seeded words");
            }
        }
    }

    #[test]
    fn gpu_flip_clock_workgroup_marker_cleanup_exact_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clocks = [original_marker_clock(&device, 21_000), GpuFlipClock::new(&device, 21_000, 1, 1)];
        let markers = [device.create_buffer_shared(21_000 * 32), device.create_buffer_shared(21_000 * 32)];
        let plan_seed = device.create_buffer_shared(48);
        let plans = [device.create_buffer_shared(48), device.create_buffer_shared(48)];
        let buffers = classify_buffers(&device);
        // Same native budget/floor populations as the CPU policy oracle.
        for (count, duration) in [(300, 0.25), (300, 1.0), (21_000, 1.0)] {
            let mut particles = vec![particle(1.25); count as usize];
            for (i, marker) in particles.iter_mut().enumerate() { marker.id = i as u32 + 101; }
            for marker in particles.iter_mut().take(9) { marker.velocity[0] = 11.0; }
            particles[9].velocity[0] = 3.25;
            for span in [1.0, 4.0] {
                let p = GpuFlipClockParams { frame_duration: duration * span,
                    limit_interval: if span == 1.0 { 0.0 } else { duration }, ..params() };
                let plan = PlanValue { dt: duration / 3.0, remaining: duration * span,
                    live_mode: 1, ..PlanValue::zeroed() };
                unsafe {
                    plan_seed.write(0, bytemuck::bytes_of(&plan));
                    for buffer in &markers { buffer.write(0, bytemuck::cast_slice(&particles)); }
                }
                let mut outputs = Vec::new();
                for (i, clock) in clocks.iter().enumerate() {
                    let mut enc = device.create_encoder("marker-workgroup-cleanup-proof");
                    enc.copy_buffer_to_buffer(&plan_seed, &clock.plan, 48);
                    enc.compute_memory_barrier_buffers();
                    clock.remove_extreme(&mut enc, &markers[i], count, &p);
                    enc.copy_buffer_to_buffer(&clock.plan, &plans[i], 48);
                    enc.copy_buffer_to_buffer(&clock.marker_histogram, &buffers[2], 256);
                    enc.copy_buffer_to_buffer(&clock.marker_outliers, &buffers[3], 16);
                    enc.commit_and_wait_completed();
                    outputs.push((read_words(&markers[i], count as usize * 8),
                        read_words(&plans[i], 12), read_words(&buffers[2], 64), read_words(&buffers[3], 4)));
                }
                assert_eq!(outputs[0], outputs[1], "{count} markers, {duration}s, {span}x span: particles/plan/histogram/outliers");
            }
        }
    }

    #[test]
    #[cfg(feature = "water-race-probes")]
    fn gpu_flip_clock_workgroup_marker_classify_bounded_timing() {
        let device = manifold_gpu::testkit::test_device();
        let buffers = classify_buffers(&device);
        let active = PlanValue { dt: 0.25, live_mode: 1, ..PlanValue::zeroed() };
        let p = GpuFlipClockParams { max_frame_steps: 64, ..params() };
        for count in [847_872u32, 6_832_128] {
            let clocks = [original_marker_clock(&device, count), GpuFlipClock::new(&device, count, 1, 1)];
            let markers = device.create_buffer_shared(u64::from(count) * 32);
            let particles: Vec<_> = (0..count).map(|i| particle(if i % 16 == 0 { 31.75 } else { 0.25 })).collect();
            unsafe { markers.write(0, bytemuck::cast_slice(&particles)); }
            drop(particles);
            let mut samples = [Vec::with_capacity(8), Vec::with_capacity(8)];
            for sample in 0..12 {
                let mut outputs = [Vec::new(), Vec::new()];
                for i in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let (words, millis) = classify_probe(&device, &clocks[i], (&markers, count), &p, &active, None, &buffers);
                    outputs[i] = words;
                    if sample >= 4 { samples[i].push(millis); }
                }
                assert_eq!(outputs[0], outputs[1], "{count} markers, sample {sample}: exact histogram/outliers");
            }
            for times in &mut samples { times.sort_by(f64::total_cmp); }
            let median = |times: &[f64]| (times[3] + times[4]) * 0.5;
            eprintln!("MARKER_CLASSIFY count={count} warm=4 measured=8 old_median_ms={:.6} workgroup_median_ms={:.6} exact=true",
                median(&samples[0]), median(&samples[1]));
        }
    }

    #[test]
    fn gpu_flip_clock_inactive_reductions_preserve_scratch_and_final_cleanup() {
        // More than one workgroup exercises both the population and partial
        // guards. The same allocations then become empty without stale maxima.
        const COUNT: u32 = 512;
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, COUNT, COUNT, COUNT);
        let markers = device.create_buffer_shared(u64::from(COUNT) * 32);
        let vertices = device.create_buffer_shared(u64::from(COUNT) * 96);
        let plan_input = device.create_buffer_shared(48);
        let plan_readback = device.create_buffer_shared(48);
        let aggregate_readback = device.create_buffer_shared(48);
        let scratch_readback = [
            device.create_buffer_shared(clock.scratch_a.size),
            device.create_buffer_shared(clock.scratch_b.size),
        ];
        let poison = device.create_buffer_shared(clock.scratch_a.size);
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
        let dispatch = |enc: &mut GpuEncoder, count| {
            clock.dispatch(enc, GpuFlipClockInputs {
                marker_particles: &markers,
                marker_count: count,
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
            }, &p);
        };
        let copy_scratch = |enc: &mut GpuEncoder| {
            for (scratch, readback) in [&clock.scratch_a, &clock.scratch_b].into_iter().zip(&scratch_readback) {
                enc.copy_buffer_to_buffer(scratch, readback, scratch.size);
            }
        };
        let assert_poison = || {
            for readback in &scratch_readback {
                assert_eq!(read_words(readback, poison_words.len()), poison_words,
                    "inactive/empty reductions must not write either scratch buffer");
            }
        };

        let mut enc = device.create_encoder("flip-clock previously nonempty populations");
        clock.begin_frame(&mut enc, &p);
        dispatch(&mut enc, COUNT);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.copy_buffer_range(&clock.obstacle_result, 0, &aggregate_readback, 16, 16);
        enc.copy_buffer_range(&clock.source_result, 0, &aggregate_readback, 32, 16);
        enc.commit_and_wait_completed();
        assert_eq!(read_plan(&plan_readback).maximum_speed, 1024.0);
        let aggregates = read_words(&aggregate_readback, 12);
        assert_eq!(f32::from_bits(aggregates[4]), 4.0);
        assert_eq!(aggregates[8], 0, "outside startup, the unused source aggregate stays cleared");

        let mut enc = device.create_encoder("flip-clock populations become empty");
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_a, poison.size);
        enc.copy_buffer_to_buffer(&poison, &clock.scratch_b, poison.size);
        clock.begin_frame(&mut enc, &p);
        dispatch(&mut enc, 0);
        for (index, aggregate) in [&clock.marker_result, &clock.obstacle_result, &clock.source_result].into_iter().enumerate() {
            enc.copy_buffer_range(aggregate, 0, &aggregate_readback, index as u64 * 16, 16);
        }
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        copy_scratch(&mut enc);
        enc.commit_and_wait_completed();
        assert_eq!(read_words(&aggregate_readback, 12), vec![0; 12]);
        let empty = read_plan(&plan_readback);
        assert_eq!(empty.maximum_speed, 0.0);
        assert_eq!(empty.dt, p.frame_duration);
        assert_poison();

        // Scheduling must stop on remaining == 0 even while the previous
        // step's dt is positive. schedule then sets dt to zero, so subsequent
        // cleanup must also leave both poisoned buffers untouched.
        let final_step = PlanValue {
            dt: 0.1, elapsed: 1.0, remaining: 0.0, live_mode: 1,
            ..PlanValue::zeroed()
        };
        // SAFETY: all earlier work has completed; plan_input is shared storage.
        unsafe { plan_input.write(0, bytemuck::bytes_of(&final_step)) };
        let mut enc = device.create_encoder("flip-clock inactive scheduling and cleanup");
        enc.copy_buffer_to_buffer(&plan_input, &clock.plan, 48);
        dispatch(&mut enc, COUNT);
        clock.remove_extreme(&mut enc, &markers, COUNT, &p);
        copy_scratch(&mut enc);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.commit_and_wait_completed();
        assert_poison();
        assert_eq!(read_plan(&plan_readback).dt, 0.0);
        let unchanged = read_words(&markers, COUNT as usize * 8);
        assert_eq!(unchanged.as_slice(), bytemuck::cast_slice::<FluidParticle, u32>(&particles));

        // The final accepted step still needs the complete reduction and
        // removal, despite having no interval remainder left to schedule.
        let mut enc = device.create_encoder("flip-clock final active cleanup");
        enc.copy_buffer_to_buffer(&plan_input, &clock.plan, 48);
        clock.remove_extreme(&mut enc, &markers, COUNT, &p);
        enc.copy_buffer_to_buffer(&clock.marker_result, &aggregate_readback, 16);
        enc.copy_buffer_to_buffer(&clock.plan, &plan_readback, 48);
        enc.commit_and_wait_completed();
        assert_eq!(f32::from_bits(read_words(&aggregate_readback, 4)[0]), 1024.0);
        let result = read_plan(&plan_readback);
        assert_eq!(result.remaining, 0.0);
        assert_eq!(result.dt, final_step.dt);
        assert_eq!(result.marker_limit, p.max_frame_steps as f32 * p.cfl * p.cell_size / p.limit_interval);
        // SAFETY: the encoder completed and the buffer holds COUNT records.
        let got = unsafe { std::slice::from_raw_parts(markers.mapped_ptr().unwrap().cast::<FluidParticle>(), COUNT as usize) };
        for (index, (before, after)) in particles.iter().zip(got).enumerate() {
            let mut want = *before;
            if index == COUNT as usize - 1 { want.position_radius[3] = 0.0; }
            assert_eq!(bytemuck::bytes_of(after), bytemuck::bytes_of(&want));
        }
    }

    #[test]
    fn gpu_flip_clock_marker_reduction_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 512, 1, 1);
        let markers = device.create_buffer_shared(512 * 32);
        let empty = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(48);
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
            let mut enc = device.create_encoder("flip-clock marker proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: count,
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
            clock.remove_extreme(&mut enc, &markers, count, &p);
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait_completed();
            let result = read_plan(&readback);
            let expected_speed = if count == 0 { 0.0 } else { speed };
            assert!((f64::from(result.maximum_speed) - expected_speed).abs() < 1e-5);
            let expected_limit = p.max_frame_steps as f32 * p.cfl * p.cell_size
                / p.limit_interval;
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
        use manifold_physics::stepping::{marker_particle_speed_limit, MarkerSpeedLimitConfig};
        let device = manifold_gpu::testkit::test_device();
        let cases = [(300u32, 0.25, 12.0), (300, 1.0, 3.0), (21_000, 1.0, 3.5)];
        for ((count, duration, limit), span) in cases.into_iter().flat_map(|case| [(case, 1.0f32), (case, 4.0)]) {
            let clock = GpuFlipClock::new(&device, count, 1, 1);
            let markers = device.create_buffer_shared(u64::from(count) * 32);
            let empty = device.create_buffer_shared(96);
            let readback = device.create_buffer_shared(48);
            let mut particles = vec![particle(1.25); count as usize];
            for (i, marker) in particles.iter_mut().enumerate() { marker.id = i as u32 + 101; }
            for marker in particles.iter_mut().take(9) {
                marker.velocity = [11.0, 0.0, 0.0];
            }
            particles[9].velocity = [3.25, 0.0, 0.0];
            unsafe {
                markers.write(0, bytemuck::cast_slice(&particles));
            }
            let limit_interval = if span == 1.0 { 0.0 } else { duration };
            let p = GpuFlipClockParams { frame_duration: duration * span, limit_interval, ..params() };
            let mut enc = device.create_encoder("flip-clock marker histogram proof");
            clock.begin_frame(&mut enc, &p);
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: count,
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
            clock.remove_extreme(&mut enc, &markers, count, &p);
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
            enc.commit_and_wait_completed();
            let result = read_plan(&readback);
            assert!((result.maximum_speed - 11.0).abs() < 1e-5);
            let speeds: Vec<f64> = particles.iter().map(|m| f64::from(m.velocity[0])).collect();
            let cpu_limit = |dt: f32| marker_particle_speed_limit(
                &speeds, Seconds(f64::from(dt)),
                f64::from(p.cell_size), f64::from(p.cfl), p.max_frame_steps,
                MarkerSpeedLimitConfig::default(), &mut [0; 6],
            ).value as f32;
            let case = format!("{count} markers, {duration} s frame, {span}x span");
            assert!(result.dt < p.frame_duration, "{case}: no CFL split");
            if span == 1.0 {
                assert_ne!(cpu_limit(result.dt), limit, "{case}: step-dt width agrees");
            } else {
                assert_ne!(cpu_limit(p.frame_duration), limit, "{case}: span width agrees");
            }
            assert_eq!(cpu_limit(duration), limit, "{case}: CPU");
            assert_eq!(result.marker_limit, limit, "{case}: GPU");
            let got = unsafe { std::slice::from_raw_parts(markers.mapped_ptr().unwrap().cast::<FluidParticle>(), count as usize) };
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
        let markers = device.create_buffer_shared(32);
        let obstacles = device.create_buffer_shared(192);
        let sources = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(48);
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
                    marker_count: 0,
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
            enc.commit_and_wait_completed();
            assert!((read_plan(&readback).maximum_speed - expected_speed).abs() < 1e-5);
        }
    }

    #[test]
    fn gpu_flip_clock_coupled_reaction_shortens_next_cfl_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let markers = device.create_buffer_shared(32);
        let obstacles = device.create_buffer_shared(96);
        let sources = device.create_buffer_shared(96);
        let rows = device.create_buffer_shared(size_of::<LiquidBody>() as u64);
        let reaction = device.create_buffer_shared(8 * size_of::<f32>() as u64);
        let readback = device.create_buffer_shared(48);
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
                    marker_count: 0,
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
            enc.commit_and_wait_completed();
            read_plan(&readback)
        };
        let without_reaction = dispatch("flip-clock coupled body zero reaction proof");
        unsafe {
            reaction.write(0, bytemuck::cast_slice(&[10.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        }
        let with_reaction = dispatch("flip-clock coupled body reaction proof");
        assert_eq!(without_reaction.maximum_speed, 0.0);
        assert_eq!(without_reaction.dt, p.frame_duration);
        assert_eq!(with_reaction.maximum_speed, 10.0);
        assert!(with_reaction.dt < without_reaction.dt);
        assert_eq!(with_reaction.dt, expected_dt(&p, 10.0, CflRestrictions::default()));
    }

    #[test]
    fn gpu_flip_clock_later_intervals_skip_source_reductions_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        // More than one workgroup also exercises the partial reduction that
        // must disappear with the unused source scan.
        const SOURCES: u32 = 129;
        let clock = GpuFlipClock::new(&device, 1, 1, SOURCES);
        let zeros = device.create_buffer_shared(128);
        zeros.zero_fill();
        let sources = device.create_buffer_shared(u64::from(SOURCES) * size_of::<GpuFlipBodyVertex>() as u64);
        let source = GpuFlipBodyVertex {
            position: [0.0, 0.0, 0.0, 1.0],
            velocity: [3.0, 0.0, 0.0, 0.0],
            ..GpuFlipBodyVertex::default()
        };
        unsafe { sources.write(0, bytemuck::cast_slice(&[source; SOURCES as usize])) };
        let inputs = |source_count| GpuFlipClockInputs {
            marker_particles: &zeros, marker_count: 0,
            obstacle_vertices: &zeros, obstacle_count: 0,
            source_vertices: &sources, source_count,
            live_hits: &zeros, live_hit_count: 0,
            event_impulses: &zeros, impulse_stride: 0,
            impulse_nodes: [2; 3], impulse_origin: [0.0; 3], impulse_spacing: 1.0,
            body_rows: &zeros, body_rows_offset: 0, body_reaction: &zeros,
        };
        let readback = device.create_buffer_shared(48);
        let initial = GpuFlipClockParams { flags: flags::FIRST_SUBSTEP, ..params() };
        let mut enc = device.create_encoder("flip-clock initial source proof");
        clock.begin_frame(&mut enc, &initial);
        let plan = clock.dispatch(&mut enc, inputs(SOURCES), &initial);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait_completed();
        assert_eq!(read_plan(&readback).maximum_speed, 3.0);

        let untouched = vec![0x12345678u32; (clock.scratch_a.size / 4) as usize];
        let seed = device.create_buffer_shared(clock.scratch_a.size);
        let scratch = device.create_buffer_shared(clock.scratch_a.size);
        unsafe { seed.write(0, bytemuck::cast_slice(&untouched)) };
        let later = params();
        let mut enc = device.create_encoder("flip-clock later source work proof");
        enc.copy_buffer_to_buffer(&seed, &clock.scratch_a, seed.size);
        clock.begin_frame(&mut enc, &later);
        let plan = clock.dispatch(&mut enc, inputs(SOURCES), &later);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.copy_buffer_to_buffer(&clock.scratch_a, &scratch, scratch.size);
        enc.commit_and_wait_completed();
        let result = read_plan(&readback);
        assert_eq!((result.maximum_speed, result.dt, result.nonfinite), (0.0, later.frame_duration, 0));
        let actual = unsafe { std::slice::from_raw_parts(scratch.mapped_ptr().unwrap().cast::<u32>(), untouched.len()) };
        assert_eq!(actual, untouched, "later intervals must not scan sources or reduce their partials");

        let mut enc = device.create_encoder("flip-clock absent source reference");
        clock.begin_frame(&mut enc, &later);
        let plan = clock.dispatch(&mut enc, inputs(0), &later);
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 48);
        enc.commit_and_wait_completed();
        assert_eq!(bytemuck::bytes_of(&result), bytemuck::bytes_of(&read_plan(&readback)));
    }

    #[test]
    fn gpu_flip_clock_source_prediction_and_restrictions_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let markers = device.create_buffer_shared(32);
        let bodies = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(48);
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
                    marker_count: 1,
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
            enc.commit_and_wait_completed();
            let result = read_plan(&readback);
            assert_eq!(result.maximum_speed, 5.0); // |source| + |force| * frame
            assert!((result.dt - expected_dt(&p, 5.0, limits)).abs() < 1e-6);
            let mut enc = device.create_encoder("flip-clock current marker proof");
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: 1,
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
            enc.commit_and_wait_completed();
            assert_eq!(read_plan(&readback).maximum_speed, 10.0);
        }
    }

    #[test]
    fn gpu_flip_clock_cap_and_nonfinite_value_proof() {
        let device = manifold_gpu::testkit::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let markers = device.create_buffer_shared(32);
        let empty = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(48);
        let before_cap = device.create_buffer_shared(48);
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
                    marker_count: 1,
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
        enc.commit_and_wait_completed();
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
                marker_count: 1,
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
        enc.commit_and_wait_completed();
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
        let before_hit = device.create_buffer_shared(32);
        let after_hit = device.create_buffer_shared(32);
        let empty = device.create_buffer_shared(96);
        let hits = device.create_buffer_shared(32);
        let impulses = device.create_buffer_shared(32 * 4);
        let readbacks: Vec<_> = (0..7).map(|_| device.create_buffer_shared(48)).collect();
        // Another interval's earlier event must neither split this interval
        // nor contribute to its predicted impulse velocity.
        let hit = [0.01_f32, 0.0, f32::from_bits(6), 0.0,
                   0.02_f32, 0.0, f32::from_bits(7), 0.0];
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
                    marker_count: 1,
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
        enc.commit_and_wait_completed();
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

    #[test]
    fn gpu_flip_clock_commit_mask_preserves_active_bits_and_offline_writes() {
        const MASK_SHADER: &str = include_str!("shaders/gpu_flip_commit_mask.wgsl");
        let device = manifold_gpu::testkit::test_device();
        let pipeline =
            device.create_compute_pipeline(MASK_SHADER, "commit_mask", "flip-clock-mask-proof");
        let plan = device.create_buffer_shared(48);
        let target = device.create_buffer_shared(32);
        let saved = device.create_buffer_shared(32);
        let active = PlanValue {
            dt: 0.25,
            elapsed: 0.25,
            remaining: 0.75,
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        let inactive = PlanValue {
            live_mode: 1,
            ..PlanValue::zeroed()
        };
        let offline = PlanValue {
            dt: 0.5,
            live_mode: 0,
            ..PlanValue::zeroed()
        };
        let original: [u32; 8] = [
            0x0000_0000,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
            0x1357_9bdf,
            0x2468_ace0,
            0xdead_beef,
            0xcafe_babe,
        ];
        let corrupted = [0xaaaaaaaau32; 8];
        let offline_bits: [u32; 8] = [
            0x1122_3344,
            0x5566_7788,
            0x99aa_bbcc,
            0xddee_ff00,
            0x0102_0304,
            0x0506_0708,
            0x0a0b_0c0d,
            0x0e0f_1011,
        ];
        let params = [8_u32, 0, 0, 0];
        unsafe {
            plan.write(0, bytemuck::bytes_of(&active));
            target.write(0, bytemuck::cast_slice(&original));
            saved.zero_fill();
        }
        let mut enc = device.create_encoder("flip-clock-mask-active");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &target,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &saved,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-mask-active",
        );
        enc.commit_and_wait_completed();
        assert_eq!(read_words(&saved, 8).as_slice(), original.as_slice());

        unsafe {
            target.write(0, bytemuck::cast_slice(&corrupted));
            plan.write(0, bytemuck::bytes_of(&inactive));
        }
        let mut enc = device.create_encoder("flip-clock-mask-inactive");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &target,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &saved,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-mask-inactive",
        );
        enc.commit_and_wait_completed();
        assert_eq!(read_words(&target, 8).as_slice(), original.as_slice());

        unsafe {
            target.write(0, bytemuck::cast_slice(&offline_bits));
            plan.write(0, bytemuck::bytes_of(&offline));
        }
        let mut enc = device.create_encoder("flip-clock-mask-offline");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &target,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &saved,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "flip-clock-mask-offline",
        );
        enc.commit_and_wait_completed();
        assert_eq!(read_words(&target, 8).as_slice(), offline_bits.as_slice());
    }
}
