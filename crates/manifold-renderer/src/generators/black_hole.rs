// Kerr black hole generator — cinematic Interstellar-style.
// Spin parameter controls frame-dragging (0 = Schwarzschild, ±1 = extremal Kerr).
//
// Two-pass compute:
//   1. Deflection map bake — volumetric geodesic trace, only on camera/param change.
//   2. Display — samples deflection map, applies cinematic disk shading.

use super::compute_common::{FIXED_POINT_SCALE, Particle};
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::BLACK_HOLE,
        create: |device| Box::new(BlackHoleGenerator::new(device)),
    }
}

const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const ROTATE: usize = 3;
const STEPS: usize = 4;
const DISK_INNER: usize = 5;
const DISK_OUTER: usize = 6;
const DISK_GLOW: usize = 7;
const SCALE: usize = 8;
const STARS: usize = 9;
const SPIN: usize = 10;
const PARTICLE_STRENGTH: usize = 11;
const PARTICLE_TURBULENCE: usize = 12;
const CAM_VELOCITY: usize = 13;
const CAM_FREEFALL: usize = 14;

const PARTICLE_COUNT: u32 = 800_000;
const POLAR_W: u32 = 2048;
const POLAR_H: u32 = 1024;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflectionUniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    uv_scale: f32,
    spin: f32,
    cam_velocity: f32,
    cam_freefall: f32,
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
    stars_brightness: f32,
    spin: f32,
    particle_strength: f32,
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
    tex_w: u32,
    tex_h: u32,
    scaled_energy: u32,
    disk_inner: f32,
    disk_outer: f32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    tex_w: u32,
    tex_h: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    width: u32,
    height: u32,
    inv2sig2: f32,
    _pad0: u32,
}

pub struct BlackHoleGenerator {
    deflection_pipeline: manifold_gpu::GpuComputePipeline,
    blur_h_pipeline: manifold_gpu::GpuComputePipeline,
    blur_v_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    particles_seed_pipeline: manifold_gpu::GpuComputePipeline,
    particles_sim_pipeline: manifold_gpu::GpuComputePipeline,
    particles_scatter_pipeline: manifold_gpu::GpuComputePipeline,
    particles_resolve_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    deflection_map: Option<manifold_gpu::GpuTexture>,
    deflection_map2: Option<manifold_gpu::GpuTexture>,
    sky_dir_map: Option<manifold_gpu::GpuTexture>,
    h_blur1: Option<manifold_gpu::GpuTexture>,
    h_blur2: Option<manifold_gpu::GpuTexture>,
    h_blur_sky: Option<manifold_gpu::GpuTexture>,
    defl_w: u32,
    defl_h: u32,

    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    scatter_accum_top: Option<manifold_gpu::GpuBuffer>,
    scatter_accum_bottom: Option<manifold_gpu::GpuBuffer>,
    polar_density_top: Option<manifold_gpu::GpuTexture>,
    polar_density_bottom: Option<manifold_gpu::GpuTexture>,
    polar_h_top: Option<manifold_gpu::GpuTexture>,
    polar_h_bot: Option<manifold_gpu::GpuTexture>,
    particles_initialized: bool,
    frame_count: u64,

    last_cam_dist: f32,
    last_tilt: f32,
    last_rotate: f32,
    last_scale: f32,
    last_steps: f32,
    last_spin: f32,
    last_cam_velocity: f32,
    last_cam_freefall: f32,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let deflection_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_deflection.wgsl"),
            "cs_main",
            "BlackHole Deflection",
        );
        let blur_src = include_str!("shaders/black_hole_blur.wgsl");
        let blur_h_pipeline =
            device.create_compute_pipeline(blur_src, "blur_h", "BlackHole Blur H");
        let blur_v_pipeline =
            device.create_compute_pipeline(blur_src, "blur_v", "BlackHole Blur V");
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_display.wgsl"),
            "cs_main",
            "BlackHole Display",
        );
        let particles_src = include_str!("shaders/black_hole_particles.wgsl");
        let particles_seed_pipeline =
            device.create_compute_pipeline(particles_src, "seed", "BlackHole Particles Seed");
        let particles_sim_pipeline =
            device.create_compute_pipeline(particles_src, "simulate", "BlackHole Particles Sim");
        let particles_scatter_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_scatter.wgsl"),
            "splat",
            "BlackHole Particles Scatter",
        );
        let particles_resolve_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_resolve.wgsl"),
            "resolve",
            "BlackHole Particles Resolve",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        Self {
            deflection_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            display_pipeline,
            particles_seed_pipeline,
            particles_sim_pipeline,
            particles_scatter_pipeline,
            particles_resolve_pipeline,
            sampler,
            deflection_map: None,
            deflection_map2: None,
            sky_dir_map: None,
            h_blur1: None,
            h_blur2: None,
            h_blur_sky: None,
            defl_w: 0,
            defl_h: 0,
            particle_buffer: None,
            scatter_accum_top: None,
            scatter_accum_bottom: None,
            polar_density_top: None,
            polar_density_bottom: None,
            polar_h_top: None,
            polar_h_bot: None,
            particles_initialized: false,
            frame_count: 0,
            last_cam_dist: f32::MIN,
            last_tilt: f32::MIN,
            last_rotate: f32::MIN,
            last_scale: f32::MIN,
            last_steps: f32::MIN,
            last_spin: f32::MIN,
            last_cam_velocity: f32::MIN,
            last_cam_freefall: f32::MIN,
        }
    }

    fn ensure_particle_resources(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.particle_buffer.is_none() {
            let buf_size = PARTICLE_COUNT as u64 * std::mem::size_of::<Particle>() as u64;
            self.particle_buffer = Some(device.create_buffer(buf_size));
        }
        let accum_size = (POLAR_W as u64) * (POLAR_H as u64) * 4;
        if self.scatter_accum_top.is_none() {
            self.scatter_accum_top = Some(device.create_buffer(accum_size));
        }
        if self.scatter_accum_bottom.is_none() {
            self.scatter_accum_bottom = Some(device.create_buffer(accum_size));
        }
        let make_density = |label| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: POLAR_W,
                height: POLAR_H,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label,
                mip_levels: 1,
            })
        };
        if self.polar_density_top.is_none() {
            self.polar_density_top = Some(make_density("BlackHole Polar Density Top"));
        }
        if self.polar_density_bottom.is_none() {
            self.polar_density_bottom = Some(make_density("BlackHole Polar Density Bottom"));
        }
        // Intermediate textures for the H pass of the polar density blur.
        if self.polar_h_top.is_none() {
            self.polar_h_top = Some(make_density("BlackHole Polar H Top"));
        }
        if self.polar_h_bot.is_none() {
            self.polar_h_bot = Some(make_density("BlackHole Polar H Bottom"));
        }
    }

    fn ensure_deflection_maps(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.deflection_map.is_some() && self.defl_w == w && self.defl_h == h {
            return;
        }
        self.defl_w = w;
        self.defl_h = h;
        let make = |label| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label,
                mip_levels: 1,
            })
        };
        self.deflection_map = Some(make("BlackHole Deflection1"));
        self.deflection_map2 = Some(make("BlackHole Deflection2"));
        self.sky_dir_map = Some(make("BlackHole SkyDir"));
        // Intermediate textures for the horizontal pass of the
        // separable gaussian. Same resolution as the bake — the blur
        // happens entirely at quarter-res so it's cheap.
        self.h_blur1 = Some(make("BlackHole H-Blur 1"));
        self.h_blur2 = Some(make("BlackHole H-Blur 2"));
        self.h_blur_sky = Some(make("BlackHole H-Blur Sky"));
        self.last_cam_dist = f32::MIN;
    }

    #[allow(clippy::too_many_arguments)]
    fn needs_rebake(
        &self,
        cd: f32,
        t: f32,
        r: f32,
        s: f32,
        st: f32,
        spin: f32,
        cam_v: f32,
        cam_ff: f32,
    ) -> bool {
        const EPS: f32 = 0.001;
        (self.last_cam_dist - cd).abs() > EPS
            || (self.last_tilt - t).abs() > EPS
            || (self.last_rotate - r).abs() > EPS
            || (self.last_scale - s).abs() > EPS
            || (self.last_steps - st).abs() > 0.5
            || (self.last_spin - spin).abs() > EPS
            || (self.last_cam_velocity - cam_v).abs() > EPS
            || (self.last_cam_freefall - cam_ff).abs() > 0.5
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
        let rotate_deg = param(ctx, ROTATE, 0.0);
        let steps = param(ctx, STEPS, 150.0).round();
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);

        let spin = param(ctx, SPIN, 0.0);
        let cam_velocity = param(ctx, CAM_VELOCITY, 0.0).clamp(0.0, 0.99);
        let cam_freefall = param(ctx, CAM_FREEFALL, 0.0);

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let tilt_rad = tilt_deg.to_radians();
        let rotate_rad = rotate_deg.to_radians();
        let orbit_angle = ctx.time as f32 * speed * 0.3;

        // Quarter-res deflection maps. Higher resolution than the
        // eighth-res experiment because mipmap box-filtering produces
        // visibly grainy blur at higher levels — finer source data
        // means lower mip levels suffice in the display sample, which
        // hides the box-filter quantization.
        let defl_w = (ctx.width / 4).max(1);
        let defl_h = (ctx.height / 4).max(1);
        self.ensure_deflection_maps(gpu.device, defl_w, defl_h);

        // ── Pass 1: Deflection Map (only on camera/scale/steps change) ──
        if self.needs_rebake(
            cam_dist,
            tilt_rad,
            rotate_rad,
            uv_scale,
            steps,
            spin,
            cam_velocity,
            cam_freefall,
        ) {
            let defl_uniforms = DeflectionUniforms {
                aspect: ctx.aspect,
                cam_dist,
                tilt_rad,
                rotate_rad,
                steps,
                uv_scale,
                spin,
                cam_velocity,
                cam_freefall,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
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
                    manifold_gpu::GpuBinding::Texture {
                        binding: 2,
                        texture: self.deflection_map2.as_ref().unwrap(),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: self.sky_dir_map.as_ref().unwrap(),
                    },
                ],
                [self.defl_w.div_ceil(16), self.defl_h.div_ceil(16), 1],
                "BlackHole Deflection",
            );

            // Separable 21-tap σ=4 gaussian blur over the deflection
            // textures. Runs entirely at quarter-res so it's cheap, but
            // the full true-gaussian quality reaches the display pass.
            // H pass: deflection_map* → h_blur*
            // V pass: h_blur* → deflection_map* (in place)
            let blur_uniforms = BlurUniforms {
                width: self.defl_w,
                height: self.defl_h,
                inv2sig2: 1.0 / 32.0, // sigma = 4
                _pad0: 0,
            };
            let blur_groups = [self.defl_w.div_ceil(16), self.defl_h.div_ceil(16), 1];

            // Horizontal pass × 3 textures
            for (src, dst, label) in [
                (
                    self.deflection_map.as_ref().unwrap(),
                    self.h_blur1.as_ref().unwrap(),
                    "BlackHole Blur H1",
                ),
                (
                    self.deflection_map2.as_ref().unwrap(),
                    self.h_blur2.as_ref().unwrap(),
                    "BlackHole Blur H2",
                ),
                (
                    self.sky_dir_map.as_ref().unwrap(),
                    self.h_blur_sky.as_ref().unwrap(),
                    "BlackHole Blur H Sky",
                ),
            ] {
                gpu.native_enc.dispatch_compute(
                    &self.blur_h_pipeline,
                    &[
                        manifold_gpu::GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&blur_uniforms),
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 1,
                            texture: src,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 2,
                            sampler: &self.sampler,
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 3,
                            texture: dst,
                        },
                    ],
                    blur_groups,
                    label,
                );
            }

            // Vertical pass × 3 textures, writing back into the deflection textures
            for (src, dst, label) in [
                (
                    self.h_blur1.as_ref().unwrap(),
                    self.deflection_map.as_ref().unwrap(),
                    "BlackHole Blur V1",
                ),
                (
                    self.h_blur2.as_ref().unwrap(),
                    self.deflection_map2.as_ref().unwrap(),
                    "BlackHole Blur V2",
                ),
                (
                    self.h_blur_sky.as_ref().unwrap(),
                    self.sky_dir_map.as_ref().unwrap(),
                    "BlackHole Blur V Sky",
                ),
            ] {
                gpu.native_enc.dispatch_compute(
                    &self.blur_v_pipeline,
                    &[
                        manifold_gpu::GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&blur_uniforms),
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 1,
                            texture: src,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 2,
                            sampler: &self.sampler,
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 3,
                            texture: dst,
                        },
                    ],
                    blur_groups,
                    label,
                );
            }

            self.last_cam_dist = cam_dist;
            self.last_tilt = tilt_rad;
            self.last_rotate = rotate_rad;
            self.last_scale = uv_scale;
            self.last_steps = steps;
            self.last_spin = spin;
            self.last_cam_velocity = cam_velocity;
            self.last_cam_freefall = cam_freefall;
        }

        // ── Pass 2: Particles (sim → scatter → resolve) ──
        let particle_strength = param(ctx, PARTICLE_STRENGTH, 0.0);
        let particle_turbulence = param(ctx, PARTICLE_TURBULENCE, 0.5);
        self.ensure_particle_resources(gpu.device);
        let part_buf = self.particle_buffer.as_ref().unwrap();
        let accum_top = self.scatter_accum_top.as_ref().unwrap();
        let accum_bot = self.scatter_accum_bottom.as_ref().unwrap();
        let polar_top = self.polar_density_top.as_ref().unwrap();
        let polar_bot = self.polar_density_bottom.as_ref().unwrap();

        let sim_uniforms = ParticleSimUniforms {
            active_count: PARTICLE_COUNT,
            frame_count: self.frame_count as u32,
            disk_inner,
            disk_outer,
            speed,
            turbulence: particle_turbulence,
            time_val: ctx.time as f32,
            dt: 1.0 / 60.0,
            inject_burst: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        if !self.particles_initialized {
            gpu.native_enc.dispatch_compute(
                &self.particles_seed_pipeline,
                &[
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 0,
                        buffer: part_buf,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 1,
                        data: bytemuck::bytes_of(&sim_uniforms),
                    },
                ],
                [PARTICLE_COUNT.div_ceil(256), 1, 1],
                "BlackHole Particles Seed",
            );
            self.particles_initialized = true;
        }

        gpu.native_enc.dispatch_compute(
            &self.particles_sim_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: part_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&sim_uniforms),
                },
            ],
            [PARTICLE_COUNT.div_ceil(256), 1, 1],
            "BlackHole Particles Sim",
        );

        gpu.native_enc.clear_buffer(accum_top);
        gpu.native_enc.clear_buffer(accum_bot);

        // Energy per particle: target peak densities ~1.0 in clumps. The
        // 4× factor compensates for the 4× larger cell count at 2048×1024
        // — without it, finer cells would each see fewer particles and
        // average density would drop proportionally.
        let energy = 0.05 * 4.0 * (1_000_000.0 / PARTICLE_COUNT as f32);
        let scaled_energy = (energy * FIXED_POINT_SCALE + 0.5) as u32;
        let scatter_uniforms = ScatterUniforms {
            active_count: PARTICLE_COUNT,
            tex_w: POLAR_W,
            tex_h: POLAR_H,
            scaled_energy,
            disk_inner,
            disk_outer,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.particles_scatter_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: part_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum_top,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum_bot,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 3,
                    data: bytemuck::bytes_of(&scatter_uniforms),
                },
            ],
            [PARTICLE_COUNT.div_ceil(256), 1, 1],
            "BlackHole Particles Scatter",
        );

        let resolve_uniforms = ResolveUniforms {
            tex_w: POLAR_W,
            tex_h: POLAR_H,
            _pad0: 0,
            _pad1: 0,
        };
        for (accum, polar, label) in [
            (accum_top, polar_top, "BlackHole Particles Resolve Top"),
            (accum_bot, polar_bot, "BlackHole Particles Resolve Bottom"),
        ] {
            gpu.native_enc.dispatch_compute(
                &self.particles_resolve_pipeline,
                &[
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 0,
                        buffer: accum,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: polar,
                    },
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 2,
                        data: bytemuck::bytes_of(&resolve_uniforms),
                    },
                ],
                [POLAR_W.div_ceil(16), POLAR_H.div_ceil(16), 1],
                label,
            );
        }

        // ── Pass 2.5: Polar density blur ──
        // Reuse the separable gaussian blur shader to smooth the polar
        // density texture before display reads it. Without this, single
        // bright cells project as visible radial wedges into the disk.
        let polar_h_top_tex = self.polar_h_top.as_ref().unwrap();
        let polar_h_bot_tex = self.polar_h_bot.as_ref().unwrap();
        let polar_blur_uniforms = BlurUniforms {
            width: POLAR_W,
            height: POLAR_H,
            inv2sig2: 1.0 / 32.0, // sigma = 4 — moderate smoothing
            _pad0: 0,
        };
        let polar_blur_groups = [POLAR_W.div_ceil(16), POLAR_H.div_ceil(16), 1];
        // H pass: polar_density → polar_h
        for (src, dst, label) in [
            (polar_top, polar_h_top_tex, "BlackHole Polar Blur H Top"),
            (polar_bot, polar_h_bot_tex, "BlackHole Polar Blur H Bottom"),
        ] {
            gpu.native_enc.dispatch_compute(
                &self.blur_h_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&polar_blur_uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: src,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: dst,
                    },
                ],
                polar_blur_groups,
                label,
            );
        }
        // V pass: polar_h → polar_density (in place)
        for (src, dst, label) in [
            (polar_h_top_tex, polar_top, "BlackHole Polar Blur V Top"),
            (polar_h_bot_tex, polar_bot, "BlackHole Polar Blur V Bottom"),
        ] {
            gpu.native_enc.dispatch_compute(
                &self.blur_v_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&polar_blur_uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: src,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: dst,
                    },
                ],
                polar_blur_groups,
                label,
            );
        }

        // ── Pass 3: Display ──
        let stars = param(ctx, STARS, 0.5);
        let display_uniforms = DisplayUniforms {
            time_val: ctx.time as f32,
            disk_inner,
            disk_outer,
            disk_glow,
            orbit_angle,
            stars_brightness: stars,
            spin,
            particle_strength,
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
                    texture: self.deflection_map2.as_ref().unwrap(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 5,
                    texture: self.sky_dir_map.as_ref().unwrap(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 6,
                    texture: polar_top,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 7,
                    texture: polar_bot,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole Display",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Deflection maps at half res — recreated in ensure_deflection_maps
    }

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
