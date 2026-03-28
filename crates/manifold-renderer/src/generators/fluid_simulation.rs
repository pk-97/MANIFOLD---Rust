// Density-displacement particle compute fluid simulation.
// Port of Unity FluidSimulationGenerator.cs — mechanical translation.
// Pipeline per frame:
//   Scatter (unified RGBA splat + resolve) ->
//   Blur density (H + V) -> GradientRotate -> Blur vector (H + V) ->
//   Simulate -> Display

use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use super::compute_common::Particle;

// Parameter indices matching types.rs param_defs (20 params)
// Unity: SLOPE=0, BLUR=1, ROTATION=2, NOISE=3, SPEED=4, CONTRAST=5,
//        INVERT=6, SCALE=7, PARTICLES=8, SNAP=9, SNAP_MODE=10,
//        SPLAT_SIZE=11, DENSITY_RES=12, DENSITY_NOISE=13, DIFFUSION=14,
//        REFRESH=15, DENSITY_REFRESH=16, COLOR_MODE=17, COLOR_BRIGHT=18, INJECT_FORCE=19
const SLOPE: usize = 0;
const BLUR: usize = 1;
const ROTATION: usize = 2;
const NOISE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
const INVERT: usize = 6;
const SCALE: usize = 7;
const PARTICLES: usize = 8;
const SNAP: usize = 9;
const SNAP_MODE: usize = 10;
const SPLAT_SIZE: usize = 11;
const DENSITY_RES: usize = 12;
const DENSITY_NOISE: usize = 13;
const DIFFUSION: usize = 14;
const REFRESH: usize = 15;
const DENSITY_REFRESH: usize = 16;
const COLOR_MODE: usize = 17;
const COLOR_BRIGHT: usize = 18;
const INJECT_FORCE: usize = 19;

// Unity constants
const MAX_PARTICLES: u32 = 8_000_000; // Unity: ParticleCount => 8000000
const PATTERN_COUNT: u32 = 7;
const SNAP_DECAY_RATE: f32 = 12.0; // ~200ms to near-zero
/// Blur/vector field resolution divider. 2 = half scatter res (matches Unity).
const PRE_SHRINK: u32 = 2;
const INJECT_FRAMES_PER_ZONE: i32 = 120; // ~2 sec at 60fps
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0; // reference for intensity normalization

// Texture formats: Rgba16Float for both density and vector field.
// Unity uses RFloat / RGFloat (R32Float / Rg32Float), but neither is filterable on Metal,
// and these textures need both storage writes (compute resolve) and filtered sampling (blur/simulate).
// Rgba16Float supports both STORAGE_BINDING and filterable sampling on Metal.
const DENSITY_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const VECTOR_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

// ── Uniform structs (matching shader layouts exactly) ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    // pre-scaled energy: 0.005 * (splat_size/3) * (1_000_000/active_count) * 4096 + 0.5
    scaled_energy: u32,
    // 0=mono, 1-5=color palette
    color_mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    // Pad to 32 bytes: wgpu/naga derives min_binding_size from the maximum
    // uniform at @group(0) @binding(2) across ALL entry points in the same
    // shader module. SplatUniforms is 32 bytes at the same binding slot,
    // so this struct must match.
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
    _pad5: u32,
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
    refresh_rate: f32,
    density_refresh_scale: f32,
    color_mode: u32,
    frame_count: u32,
    // injection point UV (random per trigger)
    inject_point_x: f32,
    inject_point_y: f32,
    inject_force: f32,
    inject_phase: f32,
    time_val: f32,
    // color index for injection (1-4 cycling)
    inject_color_index: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count: u32,
    pattern_index: u32,
    trigger_count: u32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
    color_mode: f32,
    color_bright: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct FluidSimulationGenerator {
    // Compute pipelines
    splat_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_pipeline: manifold_gpu::GpuComputePipeline,
    simulate_pipeline: manifold_gpu::GpuComputePipeline,
    seed_pipeline: manifold_gpu::GpuComputePipeline,
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    gradient_pipeline: manifold_gpu::GpuComputePipeline,
    /// Specialized display pipelines: mono (color_mode=0) vs color (color_mode=1).
    /// Metal compiler eliminates the if/else branch in each variant.
    display_pipeline_mono: manifold_gpu::GpuComputePipeline,
    display_pipeline_color: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init)
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    scatter_accum: Option<manifold_gpu::GpuBuffer>,
    density_tex: Option<manifold_gpu::GpuTexture>,
    blur_density_tex: Option<manifold_gpu::GpuTexture>,
    vector_field_tex: Option<manifold_gpu::GpuTexture>,
    blur_temp_tex: Option<manifold_gpu::GpuTexture>,

    // State
    active_count: u32,
    scatter_width: u32,
    scatter_height: u32,
    frame_count: u64,
    initialized: bool,
    current_density_res: f32,

    // Snap envelope state
    last_trigger_count: i32,
    snap_envelope: f32,
    active_snap_mode: i32,

    // Injection state machine
    last_color_mode: i32,
    inject_active: bool,
    inject_point: [f32; 2],
    inject_color_counter: u32,
    inject_frames_remaining: i32,
}

impl FluidSimulationGenerator {
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
        let display_wgsl = include_str!("shaders/fluid_display_compute.wgsl");
        let display_pipeline_mono = device.create_specialized_compute_pipeline(
            display_wgsl, "cs_main",
            &[("params.color_mode", "0.0")],
            "FluidSim Display Mono",
        );
        let display_pipeline_color = device.create_specialized_compute_pipeline(
            display_wgsl, "cs_main",
            &[("params.color_mode", "1.0")],
            "FluidSim Display Color",
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
            display_pipeline_mono,
            display_pipeline_color,
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
            last_color_mode: 0,
            inject_active: false,
            inject_point: [0.0; 2],
            inject_color_counter: 0,
            inject_frames_remaining: 0,
        }
    }

    /// Unity ComputeParticleGeneratorBase.Initialize: create and seed particle buffer.
    fn init_particles_gpu(&mut self, gpu: &mut GpuEncoder) {
        let particle_buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = gpu.device.create_buffer(
            particle_buf_size,
            manifold_gpu::GpuBufferUsage::STORAGE,
        );
        self.particle_buffer = Some(particle_buffer);
        self.initialized = true;
        self.dispatch_seed(gpu, 255, 42);
    }

    /// Unity Resize / density_res change: recreate scatter-resolution resources only.
    fn ensure_scatter_resources(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        output_width: u32,
        output_height: u32,
        density_res: f32,
    ) {
        let field_scale = density_res.clamp(0.125, 1.0);
        let sw = ((output_width as f32 * field_scale) as u32).max(64);
        let sh = ((output_height as f32 * field_scale) as u32).max(64);

        // Early out if dimensions haven't changed
        if self.scatter_accum.is_some() && self.scatter_width == sw && self.scatter_height == sh {
            return;
        }

        self.current_density_res = density_res;
        self.scatter_width = sw;
        self.scatter_height = sh;

        // Scatter accum: sw x sh x 4 channels x 4 bytes (RGBA, 4 atomic u32 per texel)
        let accum_size = (sw as u64) * (sh as u64) * 4 * 4;
        let scatter_accum = device.create_buffer(
            accum_size,
            manifold_gpu::GpuBufferUsage::STORAGE,
        );

        // Density texture: full scatter resolution
        let density_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: sw,
            height: sh,
            depth: 1,
            format: DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Density",
        });

        // Blur + vector field textures: half scatter resolution (PRE_SHRINK=2)
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
        });
        let vector_field_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: bw,
            height: bh,
            depth: 1,
            format: VECTOR_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Vector Field",
        });
        let blur_temp_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: bw,
            height: bh,
            depth: 1,
            format: VECTOR_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim Blur Temp",
        });

        self.scatter_accum = Some(scatter_accum);
        self.density_tex = Some(density_tex);
        self.blur_density_tex = Some(blur_density_tex);
        self.vector_field_tex = Some(vector_field_tex);
        self.blur_temp_tex = Some(blur_temp_tex);
        self.frame_count = 0;
    }

    fn dispatch_seed(&self, gpu: &mut GpuEncoder, pattern: u32, trigger_count: u32) {
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
}

impl Generator for FluidSimulationGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FLUID_SIMULATION
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        // Read all 20 params
        let slope = param(ctx, SLOPE, -0.01);
        let blur_radius_param = param(ctx, BLUR, 20.0);
        let rotation_deg = param(ctx, ROTATION, 85.0);
        let noise = param(ctx, NOISE, 0.001);
        let speed = param(ctx, SPEED, 1.0);
        let contrast = param(ctx, CONTRAST, 3.0);
        let invert = param(ctx, INVERT, 0.0);
        let scale = param(ctx, SCALE, 1.0);
        let particles_param = param(ctx, PARTICLES, 2.0);
        let snap = param(ctx, SNAP, 0.0);
        let snap_mode = param(ctx, SNAP_MODE, 0.0);
        let splat_size = param(ctx, SPLAT_SIZE, 3.0);
        let density_res = param(ctx, DENSITY_RES, 0.5).clamp(0.125, 1.0);
        let density_noise = param(ctx, DENSITY_NOISE, 20.0);
        let diffusion = param(ctx, DIFFUSION, 0.01);
        let refresh = param(ctx, REFRESH, 0.001);
        let density_refresh = param(ctx, DENSITY_REFRESH, 0.05);
        let color_mode_f = param(ctx, COLOR_MODE, 0.0);
        let color_bright = param(ctx, COLOR_BRIGHT, 2.0);
        let inject_force = param(ctx, INJECT_FORCE, 0.005);

        let color_mode = color_mode_f.round() as i32;

        // Unity: activeCount is dispatch-only. Buffer is always MAX_PARTICLES.
        let active_count = ((particles_param * 1_000_000.0) as u32).clamp(100_000, MAX_PARTICLES);
        self.active_count = active_count;

        // Unity: particles created once in Initialize(), never recreated for param changes.
        if !self.initialized {
            self.init_particles_gpu(gpu);
        }

        // Unity: density_res change -> Resize() which only rebuilds scatter-resolution RTs.
        self.ensure_scatter_resources(gpu.device, ctx.width, ctx.height, density_res);
        let sw = self.scatter_width;
        let sh = self.scatter_height;
        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);

        // --- Snap envelope state machine ---
        let trigger_count = ctx.trigger_count as i32;
        let mut noise_amplitude = noise;
        let mut rotation_deg_snap = rotation_deg;
        let mut slope_snap = slope;

        if trigger_count != self.last_trigger_count {
            let should_snap = snap > 0.5 && self.last_trigger_count >= 0;
            self.last_trigger_count = trigger_count;

            if should_snap {
                self.snap_envelope = 1.0;
                self.active_snap_mode = (snap_mode.round() as i32).clamp(0, 4);

                if self.active_snap_mode == 3 {
                    // Mode 3: seed pattern
                    let pattern = (trigger_count as u32) % PATTERN_COUNT;
                    self.dispatch_seed(gpu, pattern, trigger_count as u32);
                } else if self.active_snap_mode == 4 {
                    // Mode 4: inject at random point (only when color mode is active)
                    if color_mode > 0 {
                        self.inject_active = true;
                        self.inject_point = random_inject_uv(
                            trigger_count as u32,
                            self.frame_count as u32,
                        );
                        self.inject_color_counter = (self.inject_color_counter + 1) % 4;
                        self.inject_frames_remaining = INJECT_FRAMES_PER_ZONE;
                    }
                }
            }
        }

        // Decay envelope (exponential, frame-rate independent)
        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * ctx.dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        // Snap parameter overrides (scaled by decay envelope)
        if self.snap_envelope > 0.0 {
            match self.active_snap_mode {
                0 => {
                    noise_amplitude *= 1.0 + 9.0 * self.snap_envelope;
                }
                1 => {
                    rotation_deg_snap += 180.0 * self.snap_envelope;
                }
                2 => {
                    slope_snap = slope + ((-slope) - slope) * self.snap_envelope;
                }
                _ => {}
            }
        }

        // --- Color mode transition: reset injection state ---
        if color_mode == 0 && self.last_color_mode > 0 {
            self.inject_active = false;
            self.inject_frames_remaining = 0;
            self.inject_color_counter = 0;
        }
        self.last_color_mode = color_mode;

        // --- Advance injection state machine ---
        if self.inject_active {
            self.inject_frames_remaining -= 1;
            if self.inject_frames_remaining <= 0 {
                self.inject_active = false;
            }
        }

        let inject_phase = if self.inject_active {
            1.0 - (self.inject_frames_remaining as f32 / INJECT_FRAMES_PER_ZONE as f32)
        } else {
            0.0
        };
        let active_inject_force = if self.inject_active { inject_force } else { 0.0 };

        // --- Pre-computed energy for scatter ---
        let energy = 0.005 * splat_size / 3.0 * (1_000_000.0 / active_count as f32);
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

        // Clear scatter accum
        gpu.native_enc.clear_buffer(scatter_accum);

        let splat_uniforms = SplatUniforms {
            active_count,
            width: sw,
            height: sh,
            scaled_energy,
            color_mode: color_mode as u32,
            _pad0: 0, _pad1: 0, _pad2: 0,
        };
        gpu.native_enc.dispatch_compute(
            &self.splat_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0, buffer: particle_buf, offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1, buffer: scatter_accum, offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&splat_uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "FluidSim Splat",
        );

        // Resolve
        let resolve_uniforms = ResolveUniforms {
            width: sw, height: sh,
            _pad0: 0, _pad1: 0, _pad2: 0, _pad3: 0, _pad4: 0, _pad5: 0,
        };
        gpu.native_enc.dispatch_compute(
            &self.resolve_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0, buffer: scatter_accum, offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1, texture: density_tex,
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

        let base_blur_radius = blur_radius_param.round() as i32;
        let res_scale = bw as f32 / 640.0;
        let scaled_radius = (base_blur_radius as f32 * res_scale).round().max(1.0);
        let blur_texel_x = 1.0 / bw as f32;
        let blur_texel_y = 1.0 / bh as f32;

        // Downsample: density -> blur_density (radius=0)
        self.dispatch_blur(
            gpu, density_tex, blur_density_tex,
            [0.0, 0.0], 0.0, blur_texel_x, blur_texel_y, bw, bh, "FluidSim Blur Downsample",
        );

        // H blur: blur_density -> blur_temp
        self.dispatch_blur(
            gpu, blur_density_tex, blur_temp_tex,
            [1.0, 0.0], scaled_radius, blur_texel_x, blur_texel_y, bw, bh, "FluidSim Blur H",
        );

        // V blur: blur_temp -> blur_density
        self.dispatch_blur(
            gpu, blur_temp_tex, blur_density_tex,
            [0.0, 1.0], scaled_radius, blur_texel_x, blur_texel_y, bw, bh, "FluidSim Blur V",
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
            _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.gradient_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&gradient_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1, texture: blur_density_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2, texture: vector_field_tex,
                },
            ],
            [bw.div_ceil(16), bh.div_ceil(16), 1],
            "FluidSim GradientRotate",
        );

        // Blur vector field H: vector -> blur_temp
        self.dispatch_blur(
            gpu, vector_field_tex, blur_temp_tex,
            [1.0, 0.0], scaled_radius, blur_texel_x, blur_texel_y, bw, bh, "FluidSim Blur Vector H",
        );

        // Blur vector field V: blur_temp -> vector
        self.dispatch_blur(
            gpu, blur_temp_tex, vector_field_tex,
            [0.0, 1.0], scaled_radius, blur_texel_x, blur_texel_y, bw, bh, "FluidSim Blur Vector V",
        );

        // ================================================================
        // PHASE 3: Simulate
        // ================================================================

        let sim_uniforms = SimUniforms {
            active_count,
            field_width: bw,
            field_height: bh,
            speed,
            noise_amplitude,
            density_noise_gain: density_noise,
            diffusion,
            refresh_rate: refresh,
            density_refresh_scale: density_refresh,
            color_mode: color_mode as u32,
            frame_count: self.frame_count as u32,
            inject_point_x: if self.inject_active { self.inject_point[0] } else { 0.0 },
            inject_point_y: if self.inject_active { self.inject_point[1] } else { 0.0 },
            inject_force: active_inject_force,
            inject_phase,
            time_val: ctx.time as f32,
            inject_color_index: self.inject_color_counter + 1,
            _pad0: 0, _pad1: 0, _pad2: 0,
        };
        gpu.native_enc.dispatch_compute(
            &self.simulate_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0, buffer: particle_buf, offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1, texture: vector_field_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2, sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3, texture: blur_density_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 4, sampler: &self.sampler,
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
            contrast,
            invert,
            uv_scale: scale,
            color_mode: color_mode as f32,
            color_bright,
            _pad0: 0.0, _pad1: 0.0,
        };
        // Select specialized display pipeline: mono (color_mode=0) vs color (color_mode>0)
        let display_pipeline = if color_mode > 0 {
            &self.display_pipeline_color
        } else {
            &self.display_pipeline_mono
        };
        gpu.native_enc.dispatch_compute(
            display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&display_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1, texture: density_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2, sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3, texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "FluidSim Display",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Invalidate scatter resources (output dimensions changed) but keep particles alive.
        self.scatter_accum = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
    }

    /// Unity: InternalResolutionScale = 0.5 (dynamic via DENSITY_RES param).
    /// Organic particle simulation — visually indistinguishable at half res.
    fn internal_resolution_scale(&self) -> f32 {
        0.5
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        // Force full re-initialization on next render.
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
        self.inject_frames_remaining = 0;
    }
}

// ── Helpers ──

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
