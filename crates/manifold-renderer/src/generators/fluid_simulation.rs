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
use crate::render_target::RenderTarget;
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
const DENSITY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const VECTOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
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
    splat_pipeline: wgpu::ComputePipeline,
    splat_bgl: wgpu::BindGroupLayout,
    resolve_pipeline: wgpu::ComputePipeline,
    resolve_bgl: wgpu::BindGroupLayout,
    simulate_pipeline: wgpu::ComputePipeline,
    simulate_bgl: wgpu::BindGroupLayout,
    seed_pipeline: wgpu::ComputePipeline,
    seed_bgl: wgpu::BindGroupLayout,

    // Fragment pipelines (render fallback for non-hal builds)
    #[allow(dead_code)]
    blur_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    blur_vector_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    blur_bgl: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    gradient_rotate_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    gradient_rotate_bgl: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    display_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    display_bgl: wgpu::BindGroupLayout,

    // Compute pipelines for blur/gradient/display (macOS + hal-encoding)
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    blur_compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    blur_compute_bgl: wgpu::BindGroupLayout,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    gradient_compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    gradient_compute_bgl: wgpu::BindGroupLayout,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    display_compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    display_compute_bgl: wgpu::BindGroupLayout,

    // HAL pipelines for zero-overhead dispatch
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)] // infrastructure for future hal blur dispatch
    hal_blur_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)] // infrastructure for future hal gradient dispatch
    hal_gradient_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_display_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_sampler: Option<crate::hal_context::MetalSampler>,

    // GPU resources (lazy-init)
    particle_buffer: Option<wgpu::Buffer>,
    scatter_accum: Option<wgpu::Buffer>,
    density_rt: Option<RenderTarget>,
    blur_density_rt: Option<RenderTarget>,
    vector_field_rt: Option<RenderTarget>,
    blur_temp_rt: Option<RenderTarget>,

    // Uniform buffers
    splat_uniform_buf: wgpu::Buffer,
    resolve_uniform_buf: wgpu::Buffer,
    // 5 blur uniform buffers — one per blur invocation per frame. Each render pass
    // needs its own buffer because queue.write_buffer overwrites are not flushed until
    // queue.submit, so a shared buffer would only retain the last write.
    blur_uniform_bufs: [wgpu::Buffer; 5],
    gradient_uniform_buf: wgpu::Buffer,
    sim_uniform_buf: wgpu::Buffer,
    seed_uniform_buf: wgpu::Buffer,
    display_uniform_buf: wgpu::Buffer,

    sampler: wgpu::Sampler,

    // State
    active_count: u32,
    scatter_width: u32,
    scatter_height: u32,
    frame_count: u32,
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
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        // ── Scatter shader (unified RGBA splat + resolve) ──
        let scatter_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Scatter Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_scatter.wgsl").into(),
            ),
        });

        // Splat: particles(ro) + accum(rw) + uniforms
        let splat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Splat BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Splat Layout"),
            bind_group_layouts: &[&splat_bgl],
            immediate_size: 0,
        });
        let splat_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim Splat Pipeline"),
            layout: Some(&splat_layout),
            module: &scatter_shader,
            entry_point: Some("splat_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // Resolve: accum(rw) + density_out(storage_tex_write) + uniforms
        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Resolve BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: DENSITY_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let resolve_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Resolve Layout"),
            bind_group_layouts: &[&resolve_bgl],
            immediate_size: 0,
        });
        let resolve_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim Resolve Pipeline"),
            layout: Some(&resolve_layout),
            module: &scatter_shader,
            entry_point: Some("resolve_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Seed compute pipeline ──
        let seed_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Seed Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_seed.wgsl").into(),
            ),
        });
        let seed_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Seed BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(1, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let seed_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Seed Layout"),
            bind_group_layouts: &[&seed_bgl],
            immediate_size: 0,
        });
        let seed_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim Seed Pipeline"),
            layout: Some(&seed_layout),
            module: &seed_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Simulate compute pipeline ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Simulate Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_simulate.wgsl").into(),
            ),
        });
        let simulate_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Simulate BGL"),
            entries: &[
                // binding 0: particles (read_write)
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                // binding 1: vector field texture (bilinear sampling — Unity: SampleLevel)
                bgl_texture_filterable(1, wgpu::ShaderStages::COMPUTE),
                // binding 2: vector field sampler
                bgl_sampler(2, wgpu::ShaderStages::COMPUTE),
                // binding 3: density texture (bilinear sampling — Unity: SampleLevel)
                bgl_texture_filterable(3, wgpu::ShaderStages::COMPUTE),
                // binding 4: density sampler
                bgl_sampler(4, wgpu::ShaderStages::COMPUTE),
                // binding 5: uniforms
                bgl_uniform(5, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let sim_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Simulate Layout"),
            bind_group_layouts: &[&simulate_bgl],
            immediate_size: 0,
        });
        let simulate_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim Simulate Pipeline"),
            layout: Some(&sim_layout),
            module: &sim_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Gaussian blur fragment pipeline ──
        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Blur Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/gaussian_blur.wgsl").into(),
            ),
        });
        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Blur BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let blur_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Blur Layout"),
            bind_group_layouts: &[&blur_bgl],
            immediate_size: 0,
        });
        let blur_pipeline = create_fragment_pipeline(
            device, &blur_shader, &blur_layout, DENSITY_FORMAT, "FluidSim Blur (Density)",
        );
        let blur_vector_pipeline = create_fragment_pipeline(
            device, &blur_shader, &blur_layout, VECTOR_FORMAT, "FluidSim Blur (Vector)",
        );

        // ── Gradient+Rotate fragment pipeline ──
        // Uses textureLoad (no sampler) — BGL has uniform + texture only (no sampler)
        let gradient_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim GradientRotate Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_gradient_rotate.wgsl").into(),
            ),
        });
        let gradient_rotate_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim GradientRotate BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                // binding 1: density texture (textureLoad, no sampler needed)
                bgl_texture_unfilterable(1, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let gradient_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim GradientRotate Layout"),
            bind_group_layouts: &[&gradient_rotate_bgl],
            immediate_size: 0,
        });
        let gradient_rotate_pipeline = create_fragment_pipeline(
            device, &gradient_shader, &gradient_layout, VECTOR_FORMAT, "FluidSim GradientRotate",
        );

        // ── Display fragment pipeline ──
        // 5 bindings: uniforms + t_density + s_density + t_color + s_color
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Display Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_display.wgsl").into(),
            ),
        });
        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Display BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim Display Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });
        let display_pipeline = create_fragment_pipeline(
            device, &display_shader, &display_layout, target_format, "FluidSim Display",
        );

        // ── Compute pipelines for blur/gradient/display (macOS + hal-encoding) ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (
            blur_compute_pipeline,
            blur_compute_bgl,
            gradient_compute_pipeline,
            gradient_compute_bgl,
            display_compute_pipeline,
            display_compute_bgl,
        ) = {
            // Blur compute: uniform + source texture + sampler + output storage
            let blur_cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("FluidSim Blur Compute Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/gaussian_blur_compute.wgsl").into(),
                ),
            });
            let bc_bgl =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("FluidSim Blur Compute BGL"),
                    entries: &[
                        bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                        bgl_texture_filterable(1, wgpu::ShaderStages::COMPUTE),
                        bgl_sampler(2, wgpu::ShaderStages::COMPUTE),
                        bgl_storage_texture_write(
                            3,
                            wgpu::ShaderStages::COMPUTE,
                            DENSITY_FORMAT,
                        ),
                    ],
                });
            let bc_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("FluidSim Blur Compute Layout"),
                    bind_group_layouts: &[&bc_bgl],
                    immediate_size: 0,
                });
            let bc_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("FluidSim Blur Compute Pipeline"),
                    layout: Some(&bc_layout),
                    module: &blur_cs,
                    entry_point: Some("cs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });

            // Gradient compute: uniform + density texture (unfilterable) + output storage
            let gradient_cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("FluidSim GradientRotate Compute Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/fluid_gradient_rotate_compute.wgsl").into(),
                ),
            });
            let gc_bgl =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("FluidSim GradientRotate Compute BGL"),
                    entries: &[
                        bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                        bgl_texture_unfilterable(1, wgpu::ShaderStages::COMPUTE),
                        bgl_storage_texture_write(
                            2,
                            wgpu::ShaderStages::COMPUTE,
                            VECTOR_FORMAT,
                        ),
                    ],
                });
            let gc_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("FluidSim GradientRotate Compute Layout"),
                    bind_group_layouts: &[&gc_bgl],
                    immediate_size: 0,
                });
            let gc_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("FluidSim GradientRotate Compute Pipeline"),
                    layout: Some(&gc_layout),
                    module: &gradient_cs,
                    entry_point: Some("cs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });

            // Display compute: uniform + density texture + sampler + output storage
            let display_cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("FluidSim Display Compute Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/fluid_display_compute.wgsl").into(),
                ),
            });
            let dc_bgl =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("FluidSim Display Compute BGL"),
                    entries: &[
                        bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                        bgl_texture_filterable(1, wgpu::ShaderStages::COMPUTE),
                        bgl_sampler(2, wgpu::ShaderStages::COMPUTE),
                        bgl_storage_texture_write(
                            3,
                            wgpu::ShaderStages::COMPUTE,
                            wgpu::TextureFormat::Rgba16Float,
                        ),
                    ],
                });
            let dc_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("FluidSim Display Compute Layout"),
                    bind_group_layouts: &[&dc_bgl],
                    immediate_size: 0,
                });
            let dc_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("FluidSim Display Compute Pipeline"),
                    layout: Some(&dc_layout),
                    module: &display_cs,
                    entry_point: Some("cs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });

            (bc_pipeline, bc_bgl, gc_pipeline, gc_bgl, dc_pipeline, dc_bgl)
        };

        // ── HAL pipelines for zero-overhead dispatch ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_blur_pipeline, hal_gradient_pipeline, hal_display_pipeline, hal_sampler) =
            if let Some(ctx) = hal_ctx {
                use wgpu::hal::Device as HalDevice;

                // Blur HAL: uniform(dyn) + texture(filterable) + sampler + storage_tex
                let blur_bgl_entries: [wgpu::BindGroupLayoutEntry; 4] = [
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: DENSITY_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ];

                let hal_blur = crate::hal_pipeline::create_compute_pipeline(
                    ctx,
                    include_str!("shaders/gaussian_blur_compute.wgsl"),
                    "cs_main",
                    &blur_bgl_entries,
                    "FluidSim Blur HAL",
                );

                // Gradient HAL: uniform(dyn) + texture(unfilterable) + storage_tex
                let gradient_bgl_entries: [wgpu::BindGroupLayoutEntry; 3] = [
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: false,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: VECTOR_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ];

                let hal_gradient = crate::hal_pipeline::create_compute_pipeline(
                    ctx,
                    include_str!("shaders/fluid_gradient_rotate_compute.wgsl"),
                    "cs_main",
                    &gradient_bgl_entries,
                    "FluidSim Gradient HAL",
                );

                // Display HAL: uniform(dyn) + texture(filterable) + sampler + storage_tex
                let display_bgl_entries: [wgpu::BindGroupLayoutEntry; 4] = [
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba16Float,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ];

                let hal_display = crate::hal_pipeline::create_compute_pipeline(
                    ctx,
                    include_str!("shaders/fluid_display_compute.wgsl"),
                    "cs_main",
                    &display_bgl_entries,
                    "FluidSim Display HAL",
                );

                let hal_samp = unsafe {
                    ctx.device()
                        .create_sampler(&wgpu::hal::SamplerDescriptor {
                            label: Some("FluidSim Sampler HAL"),
                            address_modes: [wgpu::AddressMode::Repeat; 3],
                            mag_filter: wgpu::FilterMode::Linear,
                            min_filter: wgpu::FilterMode::Linear,
                            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                            lod_clamp: 0.0..32.0,
                            compare: None,
                            anisotropy_clamp: 1,
                            border_color: None,
                        })
                        .expect("Failed to create FluidSim hal sampler")
                };

                (
                    Some(hal_blur),
                    Some(hal_gradient),
                    Some(hal_display),
                    Some(hal_samp),
                )
            } else {
                (None, None, None, None)
            };

        // ── Uniform buffers ──
        let splat_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SplatUniforms>(), "FluidSim Splat Uniforms");
        let resolve_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ResolveUniforms>(), "FluidSim Resolve Uniforms");
        let blur_uniform_bufs = std::array::from_fn(|i| {
            create_uniform_buffer(device, std::mem::size_of::<BlurUniforms>(), &format!("FluidSim Blur Uniforms {i}"))
        });
        let gradient_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<GradientUniforms>(), "FluidSim Gradient Uniforms");
        let sim_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SimUniforms>(), "FluidSim Simulate Uniforms");
        let seed_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SeedUniforms>(), "FluidSim Seed Uniforms");
        let display_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<DisplayUniforms>(), "FluidSim Display Uniforms");

        Self {
            splat_pipeline,
            splat_bgl,
            resolve_pipeline,
            resolve_bgl,
            simulate_pipeline,
            simulate_bgl,
            seed_pipeline,
            seed_bgl,
            blur_pipeline,
            blur_vector_pipeline,
            blur_bgl,
            gradient_rotate_pipeline,
            gradient_rotate_bgl,
            display_pipeline,
            display_bgl,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            blur_compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            blur_compute_bgl,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            gradient_compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            gradient_compute_bgl,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            display_compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            display_compute_bgl,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_blur_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_gradient_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_display_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            particle_buffer: None,
            scatter_accum: None,
            density_rt: None,
            blur_density_rt: None,
            vector_field_rt: None,
            blur_temp_rt: None,
            splat_uniform_buf,
            resolve_uniform_buf,
            blur_uniform_bufs,
            gradient_uniform_buf,
            sim_uniform_buf,
            seed_uniform_buf,
            display_uniform_buf,
            sampler,
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
    /// Called once; buffer is always MAX_PARTICLES (8M). activeCount is dispatch-only.
    /// Unity NEVER recreates the particle buffer when the count slider changes.
    fn init_particles_gpu(
        &mut self,
        gpu: &mut GpuEncoder,
    ) {
        let particle_buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Particle Buffer"),
            size: particle_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.particle_buffer = Some(particle_buffer);
        self.initialized = true;
        self.dispatch_seed(gpu, 255, 42, None);
    }

    /// Unity Resize / density_res change: recreate scatter-resolution resources only.
    /// Does NOT touch the particle buffer. Unity comment: "Particle buffer is
    /// resolution-independent — no rebuild needed."
    fn ensure_scatter_resources(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
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

        // Scatter accum: sw × sh × 4 channels × 4 bytes (RGBA, 4 atomic u32 per texel)
        let accum_size = (sw as u64) * (sh as u64) * 4 * 4;
        let scatter_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Scatter Accum"),
            size: accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&scatter_accum, 0, &vec![0u8; accum_size as usize]);

        // Density RT: full scatter resolution (.r = density, .gba = pre-normalized hue)
        let density_rt = RenderTarget::new(device, sw, sh, DENSITY_FORMAT, "FluidSim Density");

        // Blur + vector field RTs: half scatter resolution (PRE_SHRINK=2)
        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);
        let blur_density_rt = RenderTarget::new(device, bw, bh, DENSITY_FORMAT, "FluidSim Blur Density");
        let vector_field_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Vector Field");
        let blur_temp_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Blur Temp");

        self.scatter_accum = Some(scatter_accum);
        self.density_rt = Some(density_rt);
        self.blur_density_rt = Some(blur_density_rt);
        self.vector_field_rt = Some(vector_field_rt);
        self.blur_temp_rt = Some(blur_temp_rt);
        self.frame_count = 0;
    }

    fn dispatch_seed(
        &self,
        gpu: &mut GpuEncoder,
        pattern: u32,
        trigger_count: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let uniforms = SeedUniforms {
            active_count: self.active_count,
            pattern_index: pattern,
            trigger_count,
            _pad0: 0,
        };
        gpu.queue.write_buffer(&self.seed_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Seed BG"),
            layout: &self.seed_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.particle_buffer.as_ref().unwrap().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.seed_uniform_buf.as_entire_binding(),
                },
            ],
        });

        let ts = profiler.and_then(|p| p.compute_timestamps("FluidSim Seed", self.active_count, 1));
        let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("FluidSim Seed Pass"),
            timestamp_writes: ts,
        });
        pass.set_pipeline(&self.seed_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(self.active_count.div_ceil(256), 1, 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_blur_pass(
        &self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        direction: [f32; 2],
        radius: f32,
        texel_x: f32,
        texel_y: f32,
        buf_index: usize,
        target_w: u32,
        target_h: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
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
        gpu.queue.write_buffer(&self.blur_uniform_bufs[buf_index], 0, bytemuck::bytes_of(&uniforms));

        // ── HAL dispatch path ───────────────────────────────────────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_blur_pipeline
            && let Some(ref hal_samp) = self.hal_sampler
            && gpu.has_hal_encoder()
        {
            use crate::hal_dispatch::*;
            use wgpu::hal::{self, Device as HalDevice};

            let offset = unsafe { gpu.uniform_arena_mut() }
                .expect("uniform_arena not set")
                .push(&uniforms);

            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            let arena_buf_ptr = unsafe { gpu.uniform_arena_mut() }
                .unwrap()
                .hal_buffer_ptr()
                .expect("arena hal buffer not available");
            let source_ptr = unsafe { extract_hal_view(source) };
            let target_ptr = unsafe { extract_hal_view(target) };
            let uniform_size = std::mem::size_of::<BlurUniforms>() as u64;

            let bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 2, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 3, resource_index: 1, count: 1 },
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            &*arena_buf_ptr,
                            0,
                            std::num::NonZero::new(uniform_size),
                        )],
                        samplers: &[hal_samp],
                        textures: &[
                            hal::TextureBinding {
                                view: &*source_ptr,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: &*target_ptr,
                                usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                            },
                        ],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect("Failed to create FluidSim Blur hal bind group")
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [target_w.div_ceil(16), target_h.div_ceil(16), 1],
                    "FluidSim Blur Compute",
                );
            }
            return;
        }

        // ── wgpu compute path (macOS + hal-encoding, hal unavailable) ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            let _ = pipeline;
            let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim Blur Compute BG"),
                layout: &self.blur_compute_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.blur_uniform_bufs[buf_index]
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(target),
                    },
                ],
            });

            let ts = profiler.and_then(|p| {
                p.compute_timestamps(
                    "FluidSim Blur Compute",
                    target_w,
                    target_h,
                )
            });
            let mut pass =
                gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim Blur Compute Pass"),
                    timestamp_writes: ts,
                });
            pass.set_pipeline(&self.blur_compute_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(
                target_w.div_ceil(16),
                target_h.div_ceil(16),
                1,
            );
        }

        // ── wgpu render path (non-macOS / no hal-encoding) ─────────────
        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            let _ = (target_w, target_h);
            let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim Blur BG"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.blur_uniform_bufs[buf_index]
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                ],
            });

            let ts = profiler.and_then(|p| {
                p.render_timestamps(
                    "FluidSim Blur",
                    self.scatter_width,
                    self.scatter_height,
                )
            });
            let mut pass =
                gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("FluidSim Blur Pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(
                                    wgpu::Color::TRANSPARENT,
                                ),
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: ts,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

impl Generator for FluidSimulationGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FLUID_SIMULATION
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
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

        // Unity: density_res change → Resize() which only rebuilds scatter-resolution RTs.
        // "Particle buffer is resolution-independent — no rebuild needed."
        self.ensure_scatter_resources(gpu.device, gpu.queue, ctx.width, ctx.height, density_res);
        let sw = self.scatter_width;
        let sh = self.scatter_height;
        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);

        // --- Snap envelope state machine ---
        // Unity: Always consume trigger transitions. Only fire if snap param is enabled.
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
                    self.dispatch_seed(gpu, pattern, trigger_count as u32, profiler);
                } else if self.active_snap_mode == 4 {
                    // Mode 4: inject at random point (only when color mode is active)
                    if color_mode > 0 {
                        self.inject_active = true;
                        self.inject_point = random_inject_uv(
                            trigger_count as u32,
                            self.frame_count,
                        );
                        self.inject_color_counter = (self.inject_color_counter + 1) % 4;
                        self.inject_frames_remaining = INJECT_FRAMES_PER_ZONE;
                    }
                }
            }
        }

        // Decay envelope (exponential, frame-rate independent)
        // Unity: snapEnvelope *= Mathf.Exp(-SNAP_DECAY_RATE * ctx.DeltaTime)
        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * ctx.dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        // Snap parameter overrides (scaled by decay envelope)
        if self.snap_envelope > 0.0 {
            match self.active_snap_mode {
                0 => {
                    // Noise blast: spike amplitude scaled by envelope
                    noise_amplitude *= 1.0 + 9.0 * self.snap_envelope;
                }
                1 => {
                    // Rotation +180 degrees
                    rotation_deg_snap += 180.0 * self.snap_envelope;
                }
                2 => {
                    // Slope flip: lerp from slope to -slope
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

        // --- Pre-computed energy for scatter (avoids shader float imprecision) ---
        // Unity: energy = 0.005 * splatSize/3 * (1_000_000/activeCount)
        let energy = 0.005 * splat_size / 3.0 * (1_000_000.0 / active_count as f32);
        let scaled_energy = (energy * 4096.0 + 0.5) as u32;

        // ================================================================
        // PHASE 1: Scatter — splat particles into accumulator
        // ================================================================

        let splat_uniforms = SplatUniforms {
            active_count,
            width: sw,
            height: sh,
            scaled_energy,
            color_mode: color_mode as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        gpu.queue.write_buffer(&self.splat_uniform_buf, 0, bytemuck::bytes_of(&splat_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let scatter_accum = self.scatter_accum.as_ref().unwrap();

        // Clear scatter accum to zero before each frame's splat
        // (atomicAdd compounds; must reset per-frame)
        gpu.encoder.clear_buffer(scatter_accum, 0, None);

        let splat_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Splat BG"),
            layout: &self.splat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.splat_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let ts = profiler.and_then(|p| p.compute_timestamps("FluidSim Splat", sw, sh));
            let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Splat Pass"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.splat_pipeline);
            pass.set_bind_group(0, &splat_bg, &[]);
            pass.dispatch_workgroups(active_count.div_ceil(256), 1, 1);
        }

        // Resolve accumulator to density texture
        let resolve_uniforms = ResolveUniforms { width: sw, height: sh, _pad0: 0, _pad1: 0, _pad2: 0, _pad3: 0, _pad4: 0, _pad5: 0 };
        gpu.queue.write_buffer(&self.resolve_uniform_buf, 0, bytemuck::bytes_of(&resolve_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();
        let resolve_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Resolve BG"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: self.resolve_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let ts = profiler.and_then(|p| p.compute_timestamps("FluidSim Resolve", sw, sh));
            let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Resolve Pass"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.dispatch_workgroups(sw.div_ceil(16), sh.div_ceil(16), 1);
        }

        // ================================================================
        // PHASE 2: Vector Field Generation
        // Blur 1 (H+V) → Gradient + Rotate → Blur 2 (H+V)
        // ================================================================

        // Unity: int baseBlurRadius = Mathf.RoundToInt(layer.GetGenParam(BLUR))
        let base_blur_radius = blur_radius_param.round() as i32;
        // Unity: float resScale = (float)blurResW / 640f
        let res_scale = bw as f32 / 640.0;
        // Unity: int scaledRadius = Mathf.Max(1, Mathf.RoundToInt(baseBlurRadius * resScale))
        let scaled_radius = (base_blur_radius as f32 * res_scale).round().max(1.0);

        // Blur texel sizes — all blur operations happen at blur resolution (bw×bh)
        let blur_texel_x = 1.0 / bw as f32;
        let blur_texel_y = 1.0 / bh as f32;

        // Unity: Graphics.Blit(densitySource, blurredDensityRT) — downsample to blur res
        // Then: ApplyGaussianBlur(blurredDensityRT, blurTempRT, scaledRadius) — in-place
        //   H: blurredDensityRT → blurTempRT, V: blurTempRT → blurredDensityRT

        // Step 1: Downsample density_rt (sw×sh) → blur_density_rt (bw×bh)
        // Use blur pass with radius=0 as a bilinear downsample blit
        let density_rt = self.density_rt.as_ref().unwrap();
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        self.run_blur_pass(gpu, &density_rt.view, &blur_density_rt.view,
            &self.blur_pipeline, [0.0, 0.0], 0.0, blur_texel_x, blur_texel_y, 0, bw, bh, profiler);

        // Step 2: H blur: blur_density_rt → blur_temp_rt
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(gpu, &blur_density_rt.view, &blur_temp_rt.view,
            &self.blur_pipeline, [1.0, 0.0], scaled_radius, blur_texel_x, blur_texel_y, 1, bw, bh, profiler);

        // Step 3: V blur: blur_temp_rt → blur_density_rt (in-place result)
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(gpu, &blur_temp_rt.view, &blur_density_rt.view,
            &self.blur_pipeline, [0.0, 1.0], scaled_radius, blur_texel_x, blur_texel_y, 2, bw, bh, profiler);

        // Gradient + Rotate: blurredDensity → vector field
        // Unity: densityAreaScale = (trailWidth * trailHeight) / SCATTER_REFERENCE_AREA
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
        gpu.queue.write_buffer(&self.gradient_uniform_buf, 0, bytemuck::bytes_of(&gradient_uniforms));

        // Unity: Graphics.Blit(blurredDensityRT, vectorFieldRT, gradientMat)
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();

        // ── macOS + hal-encoding: hal dispatch or wgpu compute ──────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_gradient_pipeline
            && gpu.has_hal_encoder()
        {
            use crate::hal_dispatch::*;
            use wgpu::hal::{self, Device as HalDevice};

            let offset = unsafe { gpu.uniform_arena_mut() }
                .expect("uniform_arena not set")
                .push(&gradient_uniforms);

            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            let arena_buf_ptr = unsafe { gpu.uniform_arena_mut() }
                .unwrap()
                .hal_buffer_ptr()
                .expect("arena hal buffer not available");
            let density_ptr = unsafe { extract_hal_view(&blur_density_rt.view) };
            let vector_ptr = unsafe { extract_hal_view(&vector_field_rt.view) };
            let uniform_size = std::mem::size_of::<GradientUniforms>() as u64;

            let bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 2, resource_index: 1, count: 1 },
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            &*arena_buf_ptr,
                            0,
                            std::num::NonZero::new(uniform_size),
                        )],
                        samplers: &[],
                        textures: &[
                            hal::TextureBinding {
                                view: &*density_ptr,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: &*vector_ptr,
                                usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                            },
                        ],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect("Failed to create FluidSim Gradient hal bind group")
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [bw.div_ceil(16), bh.div_ceil(16), 1],
                    "FluidSim GradientRotate Compute",
                );
            }
        }

        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if self.hal_gradient_pipeline.is_none() || !gpu.has_hal_encoder() {
            let gradient_bg =
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("FluidSim GradientRotate Compute BG"),
                    layout: &self.gradient_compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .gradient_uniform_buf
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                &blur_density_rt.view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(
                                &vector_field_rt.view,
                            ),
                        },
                    ],
                });
            let ts = profiler.and_then(|p| {
                p.compute_timestamps(
                    "FluidSim GradientRotate Compute",
                    bw,
                    bh,
                )
            });
            let mut pass =
                gpu.encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some(
                            "FluidSim GradientRotate Compute Pass",
                        ),
                        timestamp_writes: ts,
                    });
            pass.set_pipeline(&self.gradient_compute_pipeline);
            pass.set_bind_group(0, &gradient_bg, &[]);
            pass.dispatch_workgroups(
                bw.div_ceil(16),
                bh.div_ceil(16),
                1,
            );
        }

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            let gradient_bg =
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("FluidSim GradientRotate BG"),
                    layout: &self.gradient_rotate_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .gradient_uniform_buf
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                &blur_density_rt.view,
                            ),
                        },
                    ],
                });
            let ts = profiler.and_then(|p| {
                p.render_timestamps("FluidSim GradientRotate", bw, bh)
            });
            let mut pass =
                gpu.encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("FluidSim GradientRotate Pass"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: &vector_field_rt.view,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(
                                        wgpu::Color::TRANSPARENT,
                                    ),
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: None,
                        timestamp_writes: ts,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
            pass.set_pipeline(&self.gradient_rotate_pipeline);
            pass.set_bind_group(0, &gradient_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Blur 2: vector field (H+V) in-place via blur_temp
        // Unity: ApplyGaussianBlur(vectorFieldRT, blurTempRT, scaledRadius) — SAME radius
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(gpu, &vector_field_rt.view, &blur_temp_rt.view,
            &self.blur_vector_pipeline, [1.0, 0.0], scaled_radius, blur_texel_x, blur_texel_y, 3, bw, bh, profiler);
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(gpu, &blur_temp_rt.view, &vector_field_rt.view,
            &self.blur_vector_pipeline, [0.0, 1.0], scaled_radius, blur_texel_x, blur_texel_y, 4, bw, bh, profiler);

        // ================================================================
        // PHASE 3: Position Integration — simulate shader
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
            frame_count: self.frame_count,
            inject_point_x: if self.inject_active { self.inject_point[0] } else { 0.0 },
            inject_point_y: if self.inject_active { self.inject_point[1] } else { 0.0 },
            inject_force: active_inject_force,
            inject_phase,
            time_val: ctx.time,
            inject_color_index: self.inject_color_counter + 1,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        gpu.queue.write_buffer(&self.sim_uniform_buf, 0, bytemuck::bytes_of(&sim_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        // Unity: SetSimulationParams binds blurredDensityRT to _DensityTex
        let blurred_density_rt = self.blur_density_rt.as_ref().unwrap();

        let sim_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Simulate BG"),
            layout: &self.simulate_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vector_field_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&blurred_density_rt.view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 5, resource: self.sim_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let ts = profiler.and_then(|p| p.compute_timestamps("FluidSim Simulate", active_count, 1));
            let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Simulate Pass"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.simulate_pipeline);
            pass.set_bind_group(0, &sim_bg, &[]);
            pass.dispatch_workgroups(active_count.div_ceil(256), 1, 1);
        }

        // ================================================================
        // PHASE 4: Display — tone map density to target
        // ================================================================

        // Normalize intensity by density buffer area (Unity: 3f * areaScale)
        let area_scale = (sw as f32 * sh as f32) / SCATTER_REFERENCE_AREA;
        let intensity = 3.0 * area_scale;

        let display_uniforms = DisplayUniforms {
            intensity,
            contrast,
            invert,
            uv_scale: scale,
            color_mode: color_mode as f32,
            color_bright,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        gpu.queue.write_buffer(&self.display_uniform_buf, 0, bytemuck::bytes_of(&display_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();

        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_display_pipeline
            && let Some(ref hal_samp) = self.hal_sampler
            && gpu.has_hal_encoder()
        {
            use crate::hal_dispatch::*;
            use wgpu::hal::{self, Device as HalDevice};

            let offset = unsafe { gpu.uniform_arena_mut() }
                .expect("uniform_arena not set")
                .push(&display_uniforms);

            let (hal_enc, hal_ctx) =
                unsafe { gpu.hal_encoder_mut() }.unwrap();
            let arena_buf_ptr = unsafe { gpu.uniform_arena_mut() }
                .unwrap()
                .hal_buffer_ptr()
                .expect("arena hal buffer not available");
            let density_ptr =
                unsafe { extract_hal_view(&density_rt.view) };
            let target_ptr = unsafe { extract_hal_view(target) };
            let uniform_size =
                std::mem::size_of::<DisplayUniforms>() as u64;

            let bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry {
                                binding: 0,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 1,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 2,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 3,
                                resource_index: 1,
                                count: 1,
                            },
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            &*arena_buf_ptr,
                            0,
                            std::num::NonZero::new(uniform_size),
                        )],
                        samplers: &[hal_samp],
                        textures: &[
                            hal::TextureBinding {
                                view: &*density_ptr,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: &*target_ptr,
                                usage:
                                    wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                            },
                        ],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect(
                    "Failed to create FluidSim Display hal bind group",
                )
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [
                        ctx.width.div_ceil(16),
                        ctx.height.div_ceil(16),
                        1,
                    ],
                    "FluidSim Display Compute",
                );
            }

            self.frame_count += 1;
            return ctx.anim_progress;
        }

        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            let display_bg =
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("FluidSim Display Compute BG"),
                    layout: &self.display_compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .display_uniform_buf
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                &density_rt.view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                target,
                            ),
                        },
                    ],
                });
            let ts = profiler.and_then(|p| {
                p.compute_timestamps(
                    "FluidSim Display Compute",
                    ctx.width,
                    ctx.height,
                )
            });
            let mut pass =
                gpu.encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("FluidSim Display Compute Pass"),
                        timestamp_writes: ts,
                    });
            pass.set_pipeline(&self.display_compute_pipeline);
            pass.set_bind_group(0, &display_bg, &[]);
            pass.dispatch_workgroups(
                ctx.width.div_ceil(16),
                ctx.height.div_ceil(16),
                1,
            );
        }

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            let display_bg =
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("FluidSim Display BG"),
                    layout: &self.display_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .display_uniform_buf
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                &density_rt.view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                    ],
                });
            let ts = profiler.and_then(|p| {
                p.render_timestamps(
                    "FluidSim Display",
                    ctx.width,
                    ctx.height,
                )
            });
            let mut pass =
                gpu.encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("FluidSim Display Pass"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: target,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(
                                        wgpu::Color::TRANSPARENT,
                                    ),
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: None,
                        timestamp_writes: ts,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
            pass.set_pipeline(&self.display_pipeline);
            pass.set_bind_group(0, &display_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Invalidate scatter resources (output dimensions changed) but keep particles alive.
        // Unity Resize: "Particle buffer is resolution-independent — no rebuild needed"
        self.scatter_accum = None;
        self.scatter_width = 0;
        self.scatter_height = 0;
    }
}

// ── Helpers ──

/// Generate a random UV injection point from trigger count + frame count.
/// Wang hash for good distribution; 10% edge margin to keep bursts visible.
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

fn bgl_uniform(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_storage_rw(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_storage_ro(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_texture_filterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn bgl_texture_unfilterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn bgl_sampler(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
fn bgl_storage_texture_write(
    binding: u32,
    visibility: wgpu::ShaderStages,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn create_uniform_buffer(device: &wgpu::Device, size: usize, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_fragment_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}
