// Compute strange attractor generator — GPU-parallel particle simulation.
//
// Millions of particles trace attractor trajectories via RK2 ODE integration.
// Instantaneous positions scattered into a density field each frame (no trail decay).
// Extended Reinhard tone mapping for display.
//
// Pipeline: Simulate (compute) → Scatter (atomic) → Resolve → Display (Reinhard)
// Reuses fluid_scatter.wgsl and fluid_display_compute.wgsl for scatter/display.

use super::compute_common::Particle;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices (match generator_definition_registry order)
const TYPE: usize = 0;
const CONTRAST: usize = 1;
const CHAOS: usize = 2;
const SPEED: usize = 3;
const SCALE: usize = 4;
const SNAP: usize = 5;
const PARTICLES: usize = 6;
const DIFFUSION: usize = 7;
const TILT: usize = 8;
const SPLAT_SIZE: usize = 9;
const INVERT: usize = 10;

const ATTRACTOR_COUNT: u32 = 5;
const MAX_PARTICLES: u32 = 2_000_000;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0;

const DENSITY_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const BLUR_RADIUS: f32 = 3.0;

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
struct SimulateUniforms {
    attractor_type: u32,
    particle_count: u32,
    frame_count: u32,
    _pad0: u32,
    chaos: f32,
    cam_angle: f32,
    cam_tilt: f32,
    aspect: f32,
    diffusion: f32,
    attractor_dt: f32,
    uv_scale: f32,
    attractor_scale: f32,
    attractor_center: [f32; 3],
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    uv_scale: f32,
    invert: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction: [f32; 2],
    radius: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct StrangeAttractorGenerator {
    // Compute pipelines
    simulate_pipeline: manifold_gpu::GpuComputePipeline,
    seed_pipeline: manifold_gpu::GpuComputePipeline,
    splat_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_pipeline: manifold_gpu::GpuComputePipeline,
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init)
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    scatter_accum: Option<manifold_gpu::GpuBuffer>,
    density_tex: Option<manifold_gpu::GpuTexture>,
    blur_tex: Option<manifold_gpu::GpuTexture>,

    // State
    active_count: u32,
    scatter_width: u32,
    scatter_height: u32,
    frame_count: u64,
    initialized: bool,
    last_attractor_type: i32,
    last_trigger_count: i32,
}

impl StrangeAttractorGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let sim_src = include_str!("shaders/strange_attractor_simulate.wgsl");
        let simulate_pipeline =
            device.create_compute_pipeline(sim_src, "cs_simulate", "Attractor Simulate");
        let seed_pipeline =
            device.create_compute_pipeline(sim_src, "cs_seed", "Attractor Seed");

        let splat_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter.wgsl"),
            "splat_main",
            "Attractor Splat",
        );
        let resolve_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter.wgsl"),
            "resolve_main",
            "Attractor Resolve",
        );
        let blur_pipeline = device.create_compute_pipeline(
            include_str!("shaders/gaussian_blur_compute.wgsl"),
            "cs_main",
            "Attractor Blur",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/strange_attractor_display.wgsl"),
            "cs_main",
            "Attractor Display",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            simulate_pipeline,
            seed_pipeline,
            splat_pipeline,
            resolve_pipeline,
            blur_pipeline,
            display_pipeline,
            sampler,
            particle_buffer: None,
            scatter_accum: None,
            density_tex: None,
            blur_tex: None,
            active_count: 0,
            scatter_width: 0,
            scatter_height: 0,
            frame_count: 0,
            initialized: false,
            last_attractor_type: -1,
            last_trigger_count: -1,
        }
    }

    fn init_particles(&mut self, device: &manifold_gpu::GpuDevice) {
        let buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        self.particle_buffer = Some(device.create_buffer(buf_size));
        self.initialized = true;
    }

    fn ensure_scatter_resources(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.scatter_accum.is_some() && self.scatter_width == w && self.scatter_height == h {
            return;
        }

        self.scatter_width = w;
        self.scatter_height = h;

        let accum_size = (w as u64) * (h as u64) * 4;
        self.scatter_accum = Some(device.create_buffer(accum_size));

        self.density_tex = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Attractor Density",
        }));
        self.blur_tex = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Attractor Blur Temp",
        }));
    }

    fn dispatch_blur(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target_tex: &manifold_gpu::GpuTexture,
        direction: [f32; 2],
        radius: f32,
        texel_x: f32,
        texel_y: f32,
        target_w: u32,
        target_h: u32,
        label: &str,
    ) {
        let uniforms = BlurUniforms {
            direction,
            radius,
            texel_x,
            texel_y,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.blur_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: target_tex,
                },
            ],
            [target_w.div_ceil(16), target_h.div_ceil(16), 1],
            label,
        );
    }

    fn dispatch_seed(&self, gpu: &mut GpuEncoder, uniforms: &SimulateUniforms) {
        let particle_buf = self.particle_buffer.as_ref().unwrap();
        gpu.native_enc.dispatch_compute(
            &self.seed_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: particle_buf,
                    offset: 0,
                },
            ],
            [uniforms.particle_count.div_ceil(256), 1, 1],
            "Attractor Seed",
        );
    }

    // ── Per-type lookup tables (match Unity exactly) ──

    fn attractor_center(atype: u32) -> [f32; 3] {
        match atype {
            0 => [0.0, 0.0, 25.0],  // Lorenz
            1 => [0.0, 0.0, 2.0],   // Rössler
            2 => [0.0, 0.0, 0.5],   // Aizawa
            3 => [0.0, 0.0, 0.0],   // Thomas
            _ => [0.0, 0.0, 0.0],   // Halvorsen
        }
    }

    fn attractor_scale(atype: u32) -> f32 {
        match atype {
            0 => 25.0,
            1 => 10.0,
            2 => 1.2,
            3 => 4.0,
            _ => 12.0,
        }
    }

    fn attractor_dt(atype: u32) -> f32 {
        match atype {
            0 => 0.003,
            1 => 0.008,
            2 => 0.008,
            3 => 0.03,
            _ => 0.004,
        }
    }
}

impl Generator for StrangeAttractorGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if !self.initialized {
            self.init_particles(gpu.device);
        }

        self.ensure_scatter_resources(gpu.device, ctx.width, ctx.height);

        let particle_buf = self.particle_buffer.as_ref().unwrap();
        let scatter_accum = self.scatter_accum.as_ref().unwrap();
        let density_tex = self.density_tex.as_ref().unwrap();
        let sw = self.scatter_width;
        let sh = self.scatter_height;

        // Read params
        let anim_speed = param(ctx, SPEED, 1.0);
        let chaos = param(ctx, CHAOS, 0.0);
        let scale = param(ctx, SCALE, 1.0);
        let tilt = param(ctx, TILT, 0.3);
        let diffusion = param(ctx, DIFFUSION, 0.0);
        let contrast = param(ctx, CONTRAST, 3.5);
        let splat_size = param(ctx, SPLAT_SIZE, 3.0);
        let invert = param(ctx, INVERT, 0.0);
        let millions = param(ctx, PARTICLES, 0.5);

        // Snap mode: type cycles on trigger
        let snap = param(ctx, SNAP, 0.0) > 0.5;
        let atype = if snap {
            if ctx.trigger_count as i32 != self.last_trigger_count {
                self.last_trigger_count = ctx.trigger_count as i32;
            }
            ctx.trigger_count % ATTRACTOR_COUNT
        } else {
            if ctx.trigger_count as i32 != self.last_trigger_count {
                self.last_trigger_count = ctx.trigger_count as i32;
            }
            param(ctx, TYPE, 0.0).round().clamp(0.0, (ATTRACTOR_COUNT - 1) as f32) as u32
        };

        let active_count = (millions * 1_000_000.0)
            .round()
            .clamp(100_000.0, MAX_PARTICLES as f32) as u32;
        self.active_count = active_count;

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let sim_uniforms = SimulateUniforms {
            attractor_type: atype,
            particle_count: active_count,
            frame_count: self.frame_count as u32,
            _pad0: 0,
            chaos,
            cam_angle: ctx.time as f32 * anim_speed * 0.25,
            cam_tilt: tilt,
            aspect: ctx.aspect,
            diffusion,
            attractor_dt: Self::attractor_dt(atype) * anim_speed,
            uv_scale,
            attractor_scale: Self::attractor_scale(atype),
            attractor_center: Self::attractor_center(atype),
            _pad1: 0.0,
        };

        // Re-seed on attractor type change
        if atype as i32 != self.last_attractor_type {
            self.dispatch_seed(gpu, &sim_uniforms);
            self.last_attractor_type = atype as i32;
        }

        // ================================================================
        // PHASE 1: Simulate — RK2 ODE integration + 3D→2D projection
        // ================================================================

        gpu.native_enc.dispatch_compute(
            &self.simulate_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&sim_uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: particle_buf,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "Attractor Simulate",
        );

        // ================================================================
        // PHASE 2: Scatter — atomic density accumulation
        // ================================================================

        gpu.native_enc.clear_buffer(scatter_accum);

        let scaled_energy =
            (splat_size * super::compute_common::FIXED_POINT_SCALE).round() as u32;
        let splat_uniforms = SplatUniforms {
            active_count,
            width: sw,
            height: sh,
            scaled_energy,
        };
        gpu.native_enc.dispatch_compute(
            &self.splat_pipeline,
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
                    data: bytemuck::bytes_of(&splat_uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "Attractor Splat",
        );

        // Resolve atomics → density texture
        let resolve_uniforms = ResolveUniforms {
            width: sw,
            height: sh,
            _pad0: 0,
            _pad1: 0,
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
            [sw.div_ceil(16), sh.div_ceil(16), 1],
            "Attractor Resolve",
        );

        // ================================================================
        // PHASE 3: Blur — separable Gaussian to smooth particle density
        // ================================================================

        let blur_tex = self.blur_tex.as_ref().unwrap();
        let texel_x = 1.0 / sw as f32;
        let texel_y = 1.0 / sh as f32;

        // H blur: density_tex -> blur_tex
        self.dispatch_blur(
            gpu,
            density_tex,
            blur_tex,
            [1.0, 0.0],
            BLUR_RADIUS,
            texel_x,
            texel_y,
            sw,
            sh,
            "Attractor Blur H",
        );

        // V blur: blur_tex -> density_tex
        self.dispatch_blur(
            gpu,
            blur_tex,
            density_tex,
            [0.0, 1.0],
            BLUR_RADIUS,
            texel_x,
            texel_y,
            sw,
            sh,
            "Attractor Blur V",
        );

        // ================================================================
        // PHASE 4: Display — extended Reinhard tone mapping
        // ================================================================

        let area_scale = (sw as f32 * sh as f32) / SCATTER_REFERENCE_AREA;
        let display_uniforms = DisplayUniforms {
            intensity: 3.0 * area_scale,
            contrast,
            uv_scale: scale,
            invert,
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
                    texture: density_tex,
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
            "Attractor Display",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Invalidate scatter resources (output dimensions changed) but keep particles alive.
        self.scatter_accum = None;
        self.density_tex = None;
        self.blur_tex = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.initialized = false;
        self.frame_count = 0;
        self.particle_buffer = None;
        self.scatter_accum = None;
        self.density_tex = None;
        self.blur_tex = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
        self.last_attractor_type = -1;
        self.last_trigger_count = -1;
    }
}
