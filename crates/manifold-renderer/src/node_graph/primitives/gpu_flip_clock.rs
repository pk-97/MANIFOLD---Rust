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

/// Flags carried by [`GpuFlipClockParams::flags`].
pub mod flags {
    /// Use predicted source speed for the first substep.
    pub const FIRST_SUBSTEP: u32 = 1 << 0;
    /// Include eligible obstacle speed in the maximum.
    pub const FLUID_PRESENT_OR_GENERATING: u32 = 1 << 1;
    /// Enable the surface-tension CFL restriction.
    pub const SURFACE_TENSION: u32 = 1 << 2;
    /// Enable the source-colour mixing restriction.
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
    pub _pad0: f32,
    pub min_frame_steps: u32,
    pub max_frame_steps: u32,
    pub flags: u32,
    pub _pad1: u32,
    /// Constant acceleration added to predicted source velocity.
    pub constant_force: [f32; 4],
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
/// includes the source's fluid velocity). `position.w` is an eligibility bit;
/// noncoupled vertices outside the liquid domain have zero there. Coupled hull
/// vertices stay eligible even when outside, as in the reference engine.
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
    begin: GpuComputePipeline,
    schedule: GpuComputePipeline,
    scratch_a: GpuBuffer,
    scratch_b: GpuBuffer,
    marker_result: GpuBuffer,
    obstacle_result: GpuBuffer,
    source_result: GpuBuffer,
    plan: GpuBuffer,
}

impl GpuFlipClock {
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
            begin: device.create_compute_pipeline(SHADER, "begin_frame", "flip-clock-begin"),
            schedule: device.create_compute_pipeline(SHADER, "schedule", "flip-clock-schedule"),
            scratch_a: device.create_buffer(scratch_bytes),
            scratch_b: device.create_buffer(scratch_bytes),
            marker_result: device.create_buffer(16),
            obstacle_result: device.create_buffer(16),
            source_result: device.create_buffer(16),
            plan: device.create_buffer(32),
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
        let marker = self.reduce_marker(encoder, inputs.marker_particles, inputs.marker_count);
        encoder.copy_buffer_to_buffer(marker, &self.marker_result, 16);
        let obstacle = self.reduce_body(
            encoder,
            inputs.obstacle_vertices,
            inputs.obstacle_count,
            false,
            params,
        );
        encoder.copy_buffer_to_buffer(obstacle, &self.obstacle_result, 16);
        let source = self.reduce_body(
            encoder,
            inputs.source_vertices,
            inputs.source_count,
            true,
            params,
        );
        encoder.copy_buffer_to_buffer(source, &self.source_result, 16);

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
            ],
            [1, 1, 1],
            "flip-clock-schedule",
        );
        encoder.compute_memory_barrier_buffers();
        GpuFlipClockPlan { buffer: &self.plan }
    }

    fn reduce_marker<'a>(
        &'a self,
        encoder: &mut GpuEncoder,
        input: &GpuBuffer,
        count: u32,
    ) -> &'a GpuBuffer {
        if count == 0 {
            encoder.clear_buffer(&self.scratch_a);
            return &self.scratch_a;
        }
        let mut n = count;
        let mut pass = 0u32;
        let reduce_data = reduce_bytes(n, 0);
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
            let reduce_data = reduce_bytes(n, 0);
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
        params: &GpuFlipClockParams,
    ) -> &'a GpuBuffer {
        if count == 0 {
            encoder.clear_buffer(&self.scratch_a);
            return &self.scratch_a;
        }
        let mode = u32::from(source);
        let mut n = count;
        let mut pass = 0u32;
        let reduce_data = reduce_bytes(n, mode);
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
            let reduce_data = reduce_bytes(n, 0);
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
fn reduce_bytes(count: u32, mode: u32) -> [u8; 16] {
    let words = [count, mode, 0, 0];
    // A stack-owned inline payload; manifold-gpu consumes it during encode.
    bytemuck::cast(words)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    use crate::node_graph::fluid_particles::FluidParticle;
    use manifold_physics::Seconds;
    use manifold_physics::stepping::{CflRestrictions, cfl_step_duration};

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
            _pad0: 0.0,
            min_frame_steps: 1,
            max_frame_steps: 6,
            flags: 0,
            _pad1: 0,
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
        let bytes = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), 32) };
        bytemuck::pod_read_unaligned(bytes)
    }

    fn expected_dt(p: &GpuFlipClockParams, speed: f64, restrictions: CflRestrictions) -> f32 {
        cfl_step_duration(
            Seconds(f64::from(p.frame_duration)),
            f64::from(p.cell_size),
            f64::from(p.cfl),
            speed,
            restrictions,
        )
        .unwrap()
        .0 as f32
    }

    #[test]
    fn gpu_flip_clock_marker_reduction_value_proof() {
        let device = crate::test_device();
        let clock = GpuFlipClock::new(&device, 512, 1, 1);
        let markers = device.create_buffer_shared(512 * 32);
        let empty = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(32);
        let mut particles = [particle(1.0); 512];
        particles[511].velocity = [3.0, 4.0, 0.0];
        particles[2] = particle(1000.0);
        particles[2].position_radius[3] = 0.0;
        unsafe {
            markers.write(0, bytemuck::cast_slice(&particles));
        }
        let p = GpuFlipClockParams {
            frame_duration: 0.1,
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
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
            enc.commit_and_wait_completed();
            let result = read_plan(&readback);
            let expected_speed = if count == 0 { 0.0 } else { speed };
            assert!((f64::from(result.maximum_speed) - expected_speed).abs() < 1e-5);
            assert!(
                (result.dt - expected_dt(&p, expected_speed, CflRestrictions::default())).abs()
                    < 1e-6
            );
            assert_eq!(result.nonfinite, 0);
        }
    }

    #[test]
    fn gpu_flip_clock_obstacle_acceleration_angular_value_proof() {
        let device = crate::test_device();
        let clock = GpuFlipClock::new(&device, 1, 2, 1);
        let markers = device.create_buffer_shared(32);
        let obstacles = device.create_buffer_shared(192);
        let sources = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(32);
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
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
            enc.commit_and_wait_completed();
            assert!((read_plan(&readback).maximum_speed - expected_speed).abs() < 1e-5);
        }
    }

    #[test]
    fn gpu_flip_clock_source_prediction_and_restrictions_value_proof() {
        let device = crate::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let markers = device.create_buffer_shared(32);
        let bodies = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(32);
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
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
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
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
            enc.commit_and_wait_completed();
            assert_eq!(read_plan(&readback).maximum_speed, 10.0);
        }
    }

    #[test]
    fn gpu_flip_clock_cap_and_nonfinite_value_proof() {
        let device = crate::test_device();
        let clock = GpuFlipClock::new(&device, 1, 1, 1);
        let markers = device.create_buffer_shared(32);
        let empty = device.create_buffer_shared(96);
        let readback = device.create_buffer_shared(32);
        let p = params();
        unsafe {
            markers.write(0, bytemuck::bytes_of(&particle(1000.0)));
        }
        let mut enc = device.create_encoder("flip-clock cap proof");
        clock.begin_frame(&mut enc, &p);
        for _ in 0..p.max_frame_steps {
            let plan = clock.dispatch(
                &mut enc,
                GpuFlipClockInputs {
                    marker_particles: &markers,
                    marker_count: 1,
                    obstacle_vertices: &empty,
                    obstacle_count: 0,
                    source_vertices: &empty,
                    source_count: 0,
                },
                &p,
            );
            enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
        }
        enc.commit_and_wait_completed();
        let result = read_plan(&readback);
        let expected = 1.0 - 5.0 * expected_dt(&p, 1000.0, CflRestrictions::default());
        assert!((result.dt - expected).abs() < 1e-6);
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
            },
            &p,
        );
        enc.copy_buffer_to_buffer(plan.buffer(), &readback, 32);
        enc.commit_and_wait_completed();
        let result = read_plan(&readback);
        assert_eq!(result.dt, 1.0);
        assert_eq!(result.elapsed, 1.0);
        assert_eq!(result.remaining, 0.0);
        assert_eq!(result.nonfinite, 1);
    }
}
