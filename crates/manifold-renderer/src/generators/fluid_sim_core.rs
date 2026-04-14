//! Shared fluid simulation core — GPU resources, pipelines, and render phases.
//!
//! Owns all particle state, density/vector-field textures, and the 7 compute
//! pipelines that form the fluid simulation. Generators compose this struct
//! and drive it with their own parameter extraction + seeding logic.
//!
//! Pipeline per frame:
//!   Scatter (unified RGBA splat + resolve) ->
//!   Blur density (H + V) -> GradientRotate -> Blur vector (H + V) ->
//!   Simulate -> Display

use super::compute_common::Particle;
use crate::gpu_encoder::GpuEncoder;

// Unity constants
pub const MAX_PARTICLES: u32 = 8_000_000;
pub const PATTERN_COUNT: u32 = 7;
const SNAP_DECAY_RATE: f32 = 12.0;
/// Blur/vector field resolution divider. 2 = half scatter res (matches Unity).
const PRE_SHRINK: u32 = 2;
const INJECT_DURATION_SECS: f32 = 0.5;
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0;

const DENSITY_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const VECTOR_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

// ── Uniform structs (matching shader layouts exactly) ──

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
struct BlurUniforms {
    direction: [f32; 2],
    radius: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientUniforms {
    texel_x: f32,
    texel_y: f32,
    slope_strength: f32,
    rot_cos: f32,
    rot_sin: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    noise_amplitude: f32,
    density_noise_gain: f32,
    diffusion: f32,
    frame_count: u32,
    inject_point_x: f32,
    inject_point_y: f32,
    inject_force: f32,
    inject_phase: f32,
    time_val: f32,
    dt: f32,
    visible_count: u32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SeedUniforms {
    pub active_count: u32,
    pub pattern_index: u32,
    pub trigger_count: u32,
    pub _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    uv_scale: f32,
    _pad0: f32,
}

/// Parameters extracted by the owning generator and passed to the core each frame.
pub struct FluidSimParams {
    pub slope: f32,
    pub blur_radius: f32,
    pub rotation_deg: f32,
    pub noise: f32,
    pub speed: f32,
    pub contrast: f32,
    pub scale: f32,
    pub particles_millions: f32,
    pub snap: f32,
    pub snap_mode: f32,
    pub splat_size: f32,
    pub anti_clump: f32,
    pub inject_force: f32,
    pub fill: f32,
}

impl Default for FluidSimParams {
    fn default() -> Self {
        Self {
            slope: -0.01,
            blur_radius: 20.0,
            rotation_deg: 85.0,
            noise: 0.001,
            speed: 1.0,
            contrast: 3.0,
            scale: 1.0,
            particles_millions: 2.0,
            snap: 0.0,
            snap_mode: 0.0,
            splat_size: 3.0,
            anti_clump: 20.0,
            inject_force: 0.005,
            fill: 1.0,
        }
    }
}

/// Timing and trigger context passed each frame.
pub struct FluidSimContext {
    pub width: u32,
    pub height: u32,
    pub dt: f32,
    pub time: f64,
    pub trigger_count: u32,
}

/// Shared fluid simulation core.
pub struct FluidSimCore {
    // Compute pipelines
    splat_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_pipeline: manifold_gpu::GpuComputePipeline,
    simulate_pipeline: manifold_gpu::GpuComputePipeline,
    seed_pipeline: manifold_gpu::GpuComputePipeline,
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    gradient_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init)
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    scatter_accum: Option<manifold_gpu::GpuBuffer>,
    density_tex: Option<manifold_gpu::GpuTexture>,
    blur_density_tex: Option<manifold_gpu::GpuTexture>,
    vector_field_tex: Option<manifold_gpu::GpuTexture>,
    blur_temp_tex: Option<manifold_gpu::GpuTexture>,

    // State
    pub active_count: u32,
    scatter_width: u32,
    scatter_height: u32,
    pub frame_count: u64,
    pub initialized: bool,
    current_density_res: f32,

    // Snap envelope state
    last_trigger_count: i32,
    snap_envelope: f32,
    active_snap_mode: i32,

    // Injection state machine
    inject_active: bool,
    inject_point: [f32; 2],
    inject_elapsed: f32,
}

impl FluidSimCore {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let splat_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter.wgsl"),
            "splat_main",
            "FluidSim Splat",
        );
        let resolve_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter.wgsl"),
            "resolve_main",
            "FluidSim Resolve",
        );
        let seed_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_seed.wgsl"),
            "main",
            "FluidSim Seed",
        );
        let simulate_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_simulate.wgsl"),
            "main",
            "FluidSim Simulate",
        );
        let blur_pipeline = device.create_compute_pipeline(
            include_str!("shaders/gaussian_blur_compute.wgsl"),
            "cs_main",
            "FluidSim Blur",
        );
        let gradient_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_gradient_rotate_compute.wgsl"),
            "cs_main",
            "FluidSim Gradient",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_display_compute.wgsl"),
            "cs_main",
            "FluidSim Display",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_v: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_w: manifold_gpu::GpuAddressMode::Repeat,
            ..Default::default()
        });

        Self {
            splat_pipeline,
            resolve_pipeline,
            simulate_pipeline,
            seed_pipeline,
            blur_pipeline,
            gradient_pipeline,
            display_pipeline,
            sampler,
            particle_buffer: None,
            scatter_accum: None,
            density_tex: None,
            blur_density_tex: None,
            vector_field_tex: None,
            blur_temp_tex: None,
            active_count: 0,
            scatter_width: 0,
            scatter_height: 0,
            frame_count: 0,
            initialized: false,
            current_density_res: 0.5,
            last_trigger_count: -1,
            snap_envelope: 0.0,
            active_snap_mode: 0,
            inject_active: false,
            inject_point: [0.0; 2],
            inject_elapsed: 0.0,
        }
    }

    /// Create and seed particle buffer. Call once before first render.
    pub fn init_particles_gpu(&mut self, gpu: &mut GpuEncoder) {
        let particle_buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = gpu.device.create_buffer(particle_buf_size);
        self.particle_buffer = Some(particle_buffer);
        self.initialized = true;
        self.dispatch_seed(gpu, 255, 42);
    }

    /// Recreate scatter-resolution resources when output dimensions change.
    fn ensure_scatter_resources(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        output_width: u32,
        output_height: u32,
        density_res: f32,
    ) {
        let field_scale = density_res.clamp(0.125, 1.0);
        let sw = (((output_width as f32 * field_scale) as u32).max(640) + 1) & !1;
        let sh = (((output_height as f32 * field_scale) as u32).max(360) + 1) & !1;

        if self.scatter_accum.is_some() && self.scatter_width == sw && self.scatter_height == sh {
            return;
        }

        self.current_density_res = density_res;
        self.scatter_width = sw;
        self.scatter_height = sh;

        let accum_size = (sw as u64) * (sh as u64) * 4;
        let scatter_accum = device.create_buffer(accum_size);

        let density_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: sw,
            height: sh,
            depth: 1,
            format: DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Density",
            mip_levels: 1,
        });

        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);
        let blur_density_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: bw,
            height: bh,
            depth: 1,
            format: DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Blur Density",
            mip_levels: 1,
        });
        let vector_field_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: bw,
            height: bh,
            depth: 1,
            format: VECTOR_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Vector Field",
            mip_levels: 1,
        });
        let blur_temp_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: bw,
            height: bh,
            depth: 1,
            format: VECTOR_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Blur Temp",
            mip_levels: 1,
        });

        self.scatter_accum = Some(scatter_accum);
        self.density_tex = Some(density_tex);
        self.blur_density_tex = Some(blur_density_tex);
        self.vector_field_tex = Some(vector_field_tex);
        self.blur_temp_tex = Some(blur_temp_tex);
        self.frame_count = 0;
    }

    /// Dispatch the built-in pattern seed shader.
    pub fn dispatch_seed(&self, gpu: &mut GpuEncoder, pattern: u32, trigger_count: u32) {
        let uniforms = SeedUniforms {
            active_count: self.active_count,
            pattern_index: pattern,
            trigger_count,
            _pad0: 0,
        };

        let particle_buf = self.particle_buffer.as_ref().unwrap();
        gpu.native_enc.dispatch_compute(
            &self.seed_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: particle_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [self.active_count.div_ceil(256), 1, 1],
            "FluidSim Seed",
        );
    }

    /// Expose the particle buffer for custom seeding passes.
    pub fn particle_buffer(&self) -> &manifold_gpu::GpuBuffer {
        self.particle_buffer.as_ref().unwrap()
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

    /// Run the full fluid simulation pipeline for one frame.
    ///
    /// Call `init_particles_gpu` before the first call. Custom seeding should
    /// be dispatched after init but before this method (or between frames).
    pub fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        params: &FluidSimParams,
        ctx: &FluidSimContext,
    ) {
        let density_noise = params.anti_clump;
        let diffusion = (params.anti_clump / 60.0) * 0.05;

        let active_count =
            ((params.particles_millions * 1_000_000.0) as u32).clamp(100_000, MAX_PARTICLES);
        self.active_count = active_count;

        if !self.initialized {
            self.init_particles_gpu(gpu);
        }

        self.ensure_scatter_resources(gpu.device, ctx.width, ctx.height, 1.0);
        let sw = self.scatter_width;
        let sh = self.scatter_height;
        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);

        // --- Snap envelope state machine ---
        let trigger_count = ctx.trigger_count as i32;
        let mut noise_amplitude = params.noise;
        let mut rotation_deg_snap = params.rotation_deg;
        let mut slope_snap = params.slope;

        if trigger_count != self.last_trigger_count {
            let should_snap = params.snap > 0.5 && self.last_trigger_count >= 0;
            self.last_trigger_count = trigger_count;

            if should_snap {
                self.snap_envelope = 1.0;
                self.active_snap_mode = (params.snap_mode.round() as i32).clamp(0, 4);

                if self.active_snap_mode == 3 {
                    let pattern = (trigger_count as u32) % PATTERN_COUNT;
                    self.dispatch_seed(gpu, pattern, trigger_count as u32);
                } else if self.active_snap_mode == 4 {
                    self.inject_active = true;
                    self.inject_point =
                        random_inject_uv(trigger_count as u32, self.frame_count as u32);
                    self.inject_elapsed = 0.0;
                }
            }
        }

        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * ctx.dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        if self.snap_envelope > 0.0 {
            match self.active_snap_mode {
                0 => {
                    noise_amplitude *= 1.0 + 9.0 * self.snap_envelope;
                }
                1 => {
                    rotation_deg_snap += 180.0 * self.snap_envelope;
                }
                2 => {
                    slope_snap = params.slope
                        + ((-params.slope) - params.slope) * self.snap_envelope;
                }
                _ => {}
            }
        }

        // --- Advance injection state machine ---
        if self.inject_active {
            self.inject_elapsed += ctx.dt;
            if self.inject_elapsed >= INJECT_DURATION_SECS {
                self.inject_active = false;
            }
        }

        let inject_phase = if self.inject_active {
            self.inject_elapsed / INJECT_DURATION_SECS
        } else {
            0.0
        };
        let active_inject_force = if self.inject_active {
            params.inject_force
        } else {
            0.0
        };

        // --- Fill: visible particle count ---
        let fill = params.fill.clamp(0.0, 1.0);
        let visible_count = ((fill * active_count as f32) as u32).clamp(0, active_count);

        // --- Pre-computed energy for scatter ---
        // Scale energy by active_count (total) so density stays consistent
        let energy = 0.005 * params.splat_size / 3.0 * (1_000_000.0 / active_count as f32);
        let scaled_energy = (energy * 4096.0 + 0.5) as u32;

        let particle_buf = self.particle_buffer.as_ref().unwrap();
        let scatter_accum = self.scatter_accum.as_ref().unwrap();
        let density_tex = self.density_tex.as_ref().unwrap();
        let blur_density_tex = self.blur_density_tex.as_ref().unwrap();
        let vector_field_tex = self.vector_field_tex.as_ref().unwrap();
        let blur_temp_tex = self.blur_temp_tex.as_ref().unwrap();

        // ================================================================
        // PHASE 1: Scatter — splat particles into accumulator
        // ================================================================

        gpu.native_enc.clear_buffer(scatter_accum);

        let splat_uniforms = SplatUniforms {
            active_count: visible_count,
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
            [visible_count.div_ceil(256).max(1), 1, 1],
            "FluidSim Splat",
        );

        // Resolve
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
            "FluidSim Resolve",
        );

        // ================================================================
        // PHASE 2: Vector Field Generation
        // ================================================================

        let base_blur_radius = params.blur_radius.round() as i32;
        let blur_texel_x = 1.0 / bw as f32;
        let blur_texel_y = 1.0 / bh as f32;

        let radius_h = (base_blur_radius as f32 * bw as f32 / 640.0)
            .round()
            .max(1.0);
        let radius_v = (base_blur_radius as f32 * bh as f32 / 640.0)
            .round()
            .max(1.0);

        // Downsample: density -> blur_density (radius=0)
        self.dispatch_blur(
            gpu,
            density_tex,
            blur_density_tex,
            [0.0, 0.0],
            0.0,
            blur_texel_x,
            blur_texel_y,
            bw,
            bh,
            "FluidSim Blur Downsample",
        );

        // H blur: blur_density -> blur_temp
        self.dispatch_blur(
            gpu,
            blur_density_tex,
            blur_temp_tex,
            [1.0, 0.0],
            radius_h,
            blur_texel_x,
            blur_texel_y,
            bw,
            bh,
            "FluidSim Blur H",
        );

        // V blur: blur_temp -> blur_density
        self.dispatch_blur(
            gpu,
            blur_temp_tex,
            blur_density_tex,
            [0.0, 1.0],
            radius_v,
            blur_texel_x,
            blur_texel_y,
            bw,
            bh,
            "FluidSim Blur V",
        );

        // Gradient + Rotate
        let density_area_scale = (sw as f32 * sh as f32) / SCATTER_REFERENCE_AREA;
        let rot_rad = rotation_deg_snap * std::f32::consts::PI / 180.0;
        let gradient_uniforms = GradientUniforms {
            texel_x: blur_texel_x,
            texel_y: blur_texel_y,
            slope_strength: slope_snap * density_area_scale,
            rot_cos: rot_rad.cos(),
            rot_sin: rot_rad.sin(),
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.gradient_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&gradient_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: blur_density_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: vector_field_tex,
                },
            ],
            [bw.div_ceil(16), bh.div_ceil(16), 1],
            "FluidSim GradientRotate",
        );

        // Blur vector field H: vector -> blur_temp
        self.dispatch_blur(
            gpu,
            vector_field_tex,
            blur_temp_tex,
            [1.0, 0.0],
            radius_h,
            blur_texel_x,
            blur_texel_y,
            bw,
            bh,
            "FluidSim Blur Vector H",
        );

        // Blur vector field V: blur_temp -> vector
        self.dispatch_blur(
            gpu,
            blur_temp_tex,
            vector_field_tex,
            [0.0, 1.0],
            radius_v,
            blur_texel_x,
            blur_texel_y,
            bw,
            bh,
            "FluidSim Blur Vector V",
        );

        // ================================================================
        // PHASE 3: Simulate
        // ================================================================

        let sim_uniforms = SimUniforms {
            active_count,
            field_width: bw,
            field_height: bh,
            speed: params.speed,
            noise_amplitude,
            density_noise_gain: density_noise,
            diffusion,
            frame_count: self.frame_count as u32,
            inject_point_x: if self.inject_active {
                self.inject_point[0]
            } else {
                0.0
            },
            inject_point_y: if self.inject_active {
                self.inject_point[1]
            } else {
                0.0
            },
            inject_force: active_inject_force,
            inject_phase,
            time_val: ctx.time as f32,
            dt: ctx.dt,
            visible_count,
            _pad0: 0,
        };
        gpu.native_enc.dispatch_compute(
            &self.simulate_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: particle_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: vector_field_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: blur_density_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 4,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 5,
                    data: bytemuck::bytes_of(&sim_uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "FluidSim Simulate",
        );

        // ================================================================
        // PHASE 4: Display
        // ================================================================

        let area_scale = (sw as f32 * sh as f32) / SCATTER_REFERENCE_AREA;
        let intensity = 3.0 * area_scale;
        let display_uniforms = DisplayUniforms {
            intensity,
            contrast: params.contrast,
            uv_scale: params.scale,
            _pad0: 0.0,
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
            "FluidSim Display",
        );

        self.frame_count += 1;
    }

    /// Invalidate scatter resources (output dimensions changed) but keep particles.
    pub fn resize(&mut self) {
        self.scatter_accum = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
    }

    /// Force full re-initialization on next render.
    pub fn reset_state(&mut self) {
        self.initialized = false;
        self.frame_count = 0;
        self.particle_buffer = None;
        self.scatter_accum = None;
        self.density_tex = None;
        self.blur_density_tex = None;
        self.vector_field_tex = None;
        self.blur_temp_tex = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
        self.snap_envelope = 0.0;
        self.inject_active = false;
        self.inject_elapsed = 0.0;
    }
}

/// Generate a random UV injection point from trigger count + frame count.
fn random_inject_uv(trigger: u32, frame: u32) -> [f32; 2] {
    let seed = trigger.wrapping_mul(747796405).wrapping_add(frame);
    let mut s = (seed ^ 61) ^ (seed >> 16);
    s = s.wrapping_mul(9);
    s = s ^ (s >> 4);
    s = s.wrapping_mul(0x27d4eb2d);
    s = s ^ (s >> 15);
    let x = (s & 0xFFFF) as f32 / 65535.0;
    let y = ((s >> 16) & 0xFFFF) as f32 / 65535.0;
    [0.1 + x * 0.8, 0.1 + y * 0.8]
}
