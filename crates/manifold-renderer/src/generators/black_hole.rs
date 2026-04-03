// Schwarzschild black hole generator.
//
// Multi-pass architecture (incremental build):
//   1. Deflection map bake — geodesic trace, only on camera/param change.
//   2. Particle simulate — Schwarzschild gravity + turbulence (every frame).
//   3. Display — sample deflection map, apply disk coloring (every frame).
//
// Schwarzschild geometry is rotationally symmetric around y, so the
// deflection map is baked at orbit_angle=0 and the display shader
// offsets disk angles by the current orbit rotation.

use super::compute_common::Particle;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices (must match generator_definition_registry.rs)
const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const STEPS: usize = 3;
const DISK_INNER: usize = 4;
const DISK_OUTER: usize = 5;
const DISK_GLOW: usize = 6;
const SCALE: usize = 7;
const PARTICLE_COUNT: usize = 8;
const TURBULENCE: usize = 9;

const MAX_PARTICLES: u32 = 4_000_000;
const THREAD_GROUP_SIZE: u32 = 256;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflectionUniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    uv_scale: f32,
    _pad0: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleSimUniforms {
    active_count: u32,
    frame_count: u32,
    disk_inner: f32,
    disk_outer: f32,
    speed: f32,
    turbulence: f32,
    time_val: f32,
    dt: f32,
    inject_burst: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct BlackHoleGenerator {
    deflection_pipeline: manifold_gpu::GpuComputePipeline,
    particle_sim_pipeline: manifold_gpu::GpuComputePipeline,
    particle_seed_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // Deflection map (lazy-init, resolution-dependent)
    deflection_map: Option<manifold_gpu::GpuTexture>,
    defl_w: u32,
    defl_h: u32,

    // Particle state
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    active_count: u32,
    frame_count: u64,
    particles_initialized: bool,

    // Dirty tracking — only rebake when these change
    last_cam_dist: f32,
    last_tilt: f32,
    last_scale: f32,
    last_steps: f32,
    last_disk_inner: f32,
    last_disk_outer: f32,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let deflection_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_deflection.wgsl"),
            "cs_main",
            "BlackHole Deflection",
        );
        let particle_sim_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_particles.wgsl"),
            "simulate",
            "BlackHole ParticleSim",
        );
        let particle_seed_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_particles.wgsl"),
            "seed",
            "BlackHole ParticleSeed",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_display.wgsl"),
            "cs_main",
            "BlackHole Display",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        Self {
            deflection_pipeline,
            particle_sim_pipeline,
            particle_seed_pipeline,
            display_pipeline,
            sampler,
            deflection_map: None,
            defl_w: 0,
            defl_h: 0,
            particle_buffer: None,
            active_count: 0,
            frame_count: 0,
            particles_initialized: false,
            last_cam_dist: f32::MIN,
            last_tilt: f32::MIN,
            last_scale: f32::MIN,
            last_steps: f32::MIN,
            last_disk_inner: f32::MIN,
            last_disk_outer: f32::MIN,
        }
    }

    fn ensure_deflection_map(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.deflection_map.is_some() && self.defl_w == w && self.defl_h == h {
            return;
        }
        self.defl_w = w;
        self.defl_h = h;
        self.deflection_map = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "BlackHole DeflectionMap",
        }));
        // Force rebake on next render
        self.last_cam_dist = f32::MIN;
    }

    fn ensure_particle_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.particle_buffer.is_some() {
            return;
        }
        let buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        self.particle_buffer = Some(device.create_buffer(buf_size));
    }

    fn seed_particles(&self, gpu: &mut GpuEncoder, disk_inner: f32, disk_outer: f32) {
        let Some(ref buf) = self.particle_buffer else {
            return;
        };
        let uniforms = ParticleSimUniforms {
            active_count: self.active_count,
            frame_count: 0,
            disk_inner,
            disk_outer,
            speed: 1.0,
            turbulence: 0.3,
            time_val: 0.0,
            dt: 0.016,
            inject_burst: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.particle_seed_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [self.active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
            "BlackHole Seed",
        );
    }

    fn needs_rebake(
        &self,
        cam_dist: f32,
        tilt: f32,
        scale: f32,
        steps: f32,
        disk_inner: f32,
        disk_outer: f32,
    ) -> bool {
        const EPS: f32 = 0.001;
        (self.last_cam_dist - cam_dist).abs() > EPS
            || (self.last_tilt - tilt).abs() > EPS
            || (self.last_scale - scale).abs() > EPS
            || (self.last_steps - steps).abs() > 0.5
            || (self.last_disk_inner - disk_inner).abs() > EPS
            || (self.last_disk_outer - disk_outer).abs() > EPS
    }
}

impl Generator for BlackHoleGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::BLACK_HOLE
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if ctx.param_count == 0 {
            return ctx.anim_progress;
        }

        let speed = param(ctx, SPEED, 0.3);
        let cam_dist = param(ctx, CAM_DIST, 20.0);
        let tilt_deg = param(ctx, TILT, 75.0);
        let steps = param(ctx, STEPS, 200.0).round();
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);
        let particle_millions = param(ctx, PARTICLE_COUNT, 2.0);
        let turbulence = param(ctx, TURBULENCE, 0.3);

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let tilt_rad = tilt_deg.to_radians();
        let orbit_angle = ctx.time as f32 * speed * 0.3;
        let new_active = ((particle_millions * 1_000_000.0).round() as u32)
            .clamp(100_000, MAX_PARTICLES);

        // Ensure GPU resources
        self.ensure_deflection_map(gpu.device, ctx.width, ctx.height);
        self.ensure_particle_buffer(gpu.device);

        // Handle particle count change → reseed
        if self.active_count != new_active {
            self.active_count = new_active;
            self.particles_initialized = false;
        }

        // Seed particles on first frame or count change
        if !self.particles_initialized {
            self.seed_particles(gpu, disk_inner, disk_outer);
            self.particles_initialized = true;
        }

        // ── Pass 1: Deflection Map (only on param change) ──
        if self.needs_rebake(cam_dist, tilt_rad, uv_scale, steps, disk_inner, disk_outer) {
            let defl_uniforms = DeflectionUniforms {
                aspect: ctx.aspect,
                cam_dist,
                tilt_rad,
                steps,
                disk_inner,
                disk_outer,
                uv_scale,
                _pad0: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                &self.deflection_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&defl_uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: self.deflection_map.as_ref().unwrap(),
                    },
                ],
                [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
                "BlackHole Deflection",
            );

            self.last_cam_dist = cam_dist;
            self.last_tilt = tilt_rad;
            self.last_scale = uv_scale;
            self.last_steps = steps;
            self.last_disk_inner = disk_inner;
            self.last_disk_outer = disk_outer;
        }

        // ── Pass 2: Particle Simulate (every frame) ──
        if let Some(ref buf) = self.particle_buffer {
            let sim_uniforms = ParticleSimUniforms {
                active_count: self.active_count,
                frame_count: self.frame_count as u32,
                disk_inner,
                disk_outer,
                speed,
                turbulence,
                time_val: ctx.time as f32,
                dt: ctx.dt,
                inject_burst: 0.0,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                &self.particle_sim_pipeline,
                &[
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 0,
                        buffer: buf,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 1,
                        data: bytemuck::bytes_of(&sim_uniforms),
                    },
                ],
                [self.active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
                "BlackHole ParticleSim",
            );
            self.frame_count += 1;
        }

        // ── Pass 3: Display (every frame — cheap texture lookup) ──
        let display_uniforms = DisplayUniforms {
            time_val: ctx.time as f32,
            disk_inner,
            disk_outer,
            disk_glow,
            orbit_angle,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&display_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: self.deflection_map.as_ref().unwrap(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole Display",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.ensure_deflection_map(device, width, height);
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.particles_initialized = false;
        self.frame_count = 0;
        self.last_cam_dist = f32::MIN;
    }

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
