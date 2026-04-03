// Schwarzschild black hole with dynamic particle accretion disk.
//
// 5 compute passes:
//   [on camera/disk param change only:]
//   1. Deflection map bake — geodesic trace, stores ray termination data
//   [every frame:]
//   2. Particle simulate — Schwarzschild gravity + curl-noise turbulence
//   3. Particle scatter — splat to 2D polar disk density (atomic)
//   4. Scatter resolve — atomic accum → RGBA density texture + self-clear
//   5. Display — sample deflection map + disk density → final output

use super::compute_common::Particle;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

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
const FIXED_POINT_SCALE: f32 = 4096.0;
const FOV_FACTOR: f32 = 1.2;

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
struct ScatterUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    scaled_energy: u32,
    cam_pos: [f32; 3],
    _pad0: f32,
    cam_fwd: [f32; 3],
    _pad1: f32,
    cam_right: [f32; 3],
    _pad2: f32,
    cam_up: [f32; 3],
    fov_factor: f32,
    aspect: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    tex_w: u32,
    tex_h: u32,
    disk_inner: f32,
    disk_outer: f32,
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
    // Pipelines
    deflection_pipeline: manifold_gpu::GpuComputePipeline,
    particle_sim_pipeline: manifold_gpu::GpuComputePipeline,
    particle_seed_pipeline: manifold_gpu::GpuComputePipeline,
    scatter_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init)
    deflection_map: Option<manifold_gpu::GpuTexture>,
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    scatter_accum: Option<manifold_gpu::GpuBuffer>,
    disk_density_tex: Option<manifold_gpu::GpuTexture>,

    // State
    defl_w: u32,
    defl_h: u32,
    active_count: u32,
    frame_count: u64,
    particles_initialized: bool,

    // Dirty tracking
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
        let scatter_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_scatter.wgsl"),
            "splat",
            "BlackHole Scatter",
        );
        let resolve_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_resolve.wgsl"),
            "resolve",
            "BlackHole Resolve",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_display.wgsl"),
            "cs_main",
            "BlackHole Display",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
            ..Default::default()
        });

        Self {
            deflection_pipeline,
            particle_sim_pipeline,
            particle_seed_pipeline,
            scatter_pipeline,
            resolve_pipeline,
            display_pipeline,
            sampler,
            deflection_map: None,
            particle_buffer: None,
            scatter_accum: None,
            disk_density_tex: None,
            defl_w: 0,
            defl_h: 0,
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
        self.last_cam_dist = f32::MIN;
    }

    fn ensure_particle_resources(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) {
        if self.particle_buffer.is_none() {
            self.particle_buffer =
                Some(device.create_buffer(MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES));
        }
        // Scatter accum + density tex must match render resolution
        let need_resize =
            self.scatter_accum.is_none() || self.defl_w != w || self.defl_h != h;
        if need_resize {
            self.scatter_accum =
                Some(device.create_buffer((w as u64) * (h as u64) * 4));
            self.disk_density_tex = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label: "BlackHole ParticleDensity",
            }));
        }
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

        // ── Ensure GPU resources ──
        self.ensure_deflection_map(gpu.device, ctx.width, ctx.height);
        self.ensure_particle_resources(gpu.device, ctx.width, ctx.height);

        if self.active_count != new_active {
            self.active_count = new_active;
            self.particles_initialized = false;
        }

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

        // ── Pass 2: Particle Simulate ──
        let particle_buf = self.particle_buffer.as_ref().unwrap();
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
                    buffer: particle_buf,
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

        // ── Pass 3: Scatter particles to screen-space density ──
        // Compute camera vectors (must match deflection shader's camera at orbit_angle)
        let cos_tilt = tilt_rad.cos();
        let sin_tilt = tilt_rad.sin();
        let cos_orbit = orbit_angle.cos();
        let sin_orbit = orbit_angle.sin();

        let cam_pos = [
            cam_dist * cos_tilt * cos_orbit,
            cam_dist * sin_tilt,
            cam_dist * cos_tilt * sin_orbit,
        ];
        let cam_len = (cam_pos[0] * cam_pos[0]
            + cam_pos[1] * cam_pos[1]
            + cam_pos[2] * cam_pos[2])
            .sqrt();
        let fwd = [
            -cam_pos[0] / cam_len,
            -cam_pos[1] / cam_len,
            -cam_pos[2] / cam_len,
        ];
        // right = normalize(cross(fwd, world_up))
        let world_up = [0.0_f32, 1.0, 0.0];
        let rx = fwd[1] * world_up[2] - fwd[2] * world_up[1];
        let ry = fwd[2] * world_up[0] - fwd[0] * world_up[2];
        let rz = fwd[0] * world_up[1] - fwd[1] * world_up[0];
        let rlen = (rx * rx + ry * ry + rz * rz).sqrt();
        let right = [rx / rlen, ry / rlen, rz / rlen];
        // up = cross(right, fwd)
        let up = [
            right[1] * fwd[2] - right[2] * fwd[1],
            right[2] * fwd[0] - right[0] * fwd[2],
            right[0] * fwd[1] - right[1] * fwd[0],
        ];

        let scatter_accum = self.scatter_accum.as_ref().unwrap();
        let reference_count = 2_000_000.0_f32;
        let energy_per_particle = reference_count / self.active_count as f32;
        let scaled_energy = (energy_per_particle * FIXED_POINT_SCALE * 0.5) as u32;

        let scatter_uniforms = ScatterUniforms {
            active_count: self.active_count,
            disp_w: ctx.width,
            disp_h: ctx.height,
            scaled_energy: scaled_energy.max(1),
            cam_pos,
            _pad0: 0.0,
            cam_fwd: fwd,
            _pad1: 0.0,
            cam_right: right,
            _pad2: 0.0,
            cam_up: up,
            fov_factor: FOV_FACTOR,
            aspect: ctx.aspect,
            _pad3: 0.0,
            _pad4: 0.0,
            _pad5: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.scatter_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: particle_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: scatter_accum,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&scatter_uniforms),
                },
            ],
            [self.active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
            "BlackHole Scatter",
        );

        // ── Pass 4: Resolve scatter → density texture ──
        let density_tex = self.disk_density_tex.as_ref().unwrap();
        let resolve_uniforms = ResolveUniforms {
            tex_w: ctx.width,
            tex_h: ctx.height,
            disk_inner,
            disk_outer,
        };
        gpu.native_enc.dispatch_compute(
            &self.resolve_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: scatter_accum,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: density_tex,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&resolve_uniforms),
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole Resolve",
        );

        // ── Pass 5: Display ──
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
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: density_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
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
