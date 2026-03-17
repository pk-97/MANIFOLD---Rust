// Density-displacement particle compute fluid simulation.
// Port of Unity FluidSimulationGenerator.cs — mechanical translation.
// Pipeline per frame:
//   Scatter (splat + resolve) -> [Color scatter (splat_color + resolve_color)] ->
//   Blur density (H + V) -> GradientRotate -> Blur vector (H + V) ->
//   Simulate -> Display

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
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
const PATTERN_COUNT: u32 = 7;
const SNAP_DECAY_RATE: f32 = 12.0; // ~200ms to near-zero
const PRE_SHRINK: u32 = 2;
const INJECT_FRAMES_PER_ZONE: i32 = 120; // ~2 sec at 60fps
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0; // reference for intensity normalization

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
struct SplatColorUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
    color_mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveColorUniforms {
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
    // noise amplitude (NoiseAmplitude)
    noise_amplitude: f32,
    // density noise gain (DensityNoiseGain)
    density_noise_gain: f32,
    // diffusion amount (Diffusion)
    diffusion: f32,
    // per-frame respawn probability (RefreshRate)
    refresh_rate: f32,
    // extra respawn in dense regions (DensityRefreshScale)
    density_refresh_scale: f32,
    // color mode: 0=mono, >0=inject
    color_mode: u32,
    // monotonic frame counter
    frame_count: u32,
    // -1 = off, 0-3 = active zone index
    inject_index: i32,
    // injection force strength
    inject_force: f32,
    // injection burst progress 0->1
    inject_phase: f32,
    // clip-relative time for noise evolution
    time_val: f32,
    _pad: f32,
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
    splat_color_pipeline: wgpu::ComputePipeline,
    splat_color_bgl: wgpu::BindGroupLayout,
    resolve_color_pipeline: wgpu::ComputePipeline,
    resolve_color_bgl: wgpu::BindGroupLayout,
    simulate_pipeline: wgpu::ComputePipeline,
    simulate_bgl: wgpu::BindGroupLayout,
    seed_pipeline: wgpu::ComputePipeline,
    seed_bgl: wgpu::BindGroupLayout,

    // Fragment pipelines
    blur_pipeline: wgpu::RenderPipeline,
    blur_vector_pipeline: wgpu::RenderPipeline,
    blur_bgl: wgpu::BindGroupLayout,
    gradient_rotate_pipeline: wgpu::RenderPipeline,
    gradient_rotate_bgl: wgpu::BindGroupLayout,
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,

    // GPU resources (lazy-init)
    particle_buffer: Option<wgpu::Buffer>,
    scatter_accum: Option<wgpu::Buffer>,
    color_accum: Option<wgpu::Buffer>,
    density_rt: Option<RenderTarget>,
    blur_density_rt: Option<RenderTarget>,
    vector_field_rt: Option<RenderTarget>,
    blur_temp_rt: Option<RenderTarget>,
    color_density_rt: Option<RenderTarget>,

    // Uniform buffers
    splat_uniform_buf: wgpu::Buffer,
    resolve_uniform_buf: wgpu::Buffer,
    splat_color_uniform_buf: wgpu::Buffer,
    resolve_color_uniform_buf: wgpu::Buffer,
    blur_uniform_buf: wgpu::Buffer,
    gradient_uniform_buf: wgpu::Buffer,
    sim_uniform_buf: wgpu::Buffer,
    seed_uniform_buf: wgpu::Buffer,
    display_uniform_buf: wgpu::Buffer,

    // White texture fallback for display pass when color_density_rt is None
    white_texture: wgpu::Texture,
    white_view: wgpu::TextureView,

    sampler: wgpu::Sampler,

    // State
    active_count: u32,
    scatter_width: u32,
    scatter_height: u32,
    frame_count: u32,
    initialized: bool,
    current_density_res: f32,

    // Snap envelope state (Unity: snapEnvelope, activeSnapMode, lastTriggerCount)
    last_trigger_count: i32,
    snap_envelope: f32,
    active_snap_mode: i32,

    // Injection zone state machine (Unity: injectZoneIndex, injectFramesRemaining, nextInjectZone)
    last_color_mode: i32,
    inject_zone_index: i32,   // -1 = inactive, 0-3 = current zone
    inject_frames_remaining: i32,
    next_inject_zone: i32,
}

impl FluidSimulationGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        // ── Scatter shader (splat + resolve + splat_color + resolve_color) ──
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

        // SplatColor: color_particles(ro) + color_accum(rw) + uniforms
        let splat_color_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim SplatColor BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_color_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim SplatColor Layout"),
            bind_group_layouts: &[&splat_color_bgl],
            immediate_size: 0,
        });
        let splat_color_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim SplatColor Pipeline"),
            layout: Some(&splat_color_layout),
            module: &scatter_shader,
            entry_point: Some("splat_color_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ResolveColor: color_resolve_accum(rw) + color_density_out(storage_tex_write) + uniforms
        let resolve_color_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim ResolveColor BGL"),
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
        let resolve_color_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim ResolveColor Layout"),
            bind_group_layouts: &[&resolve_color_bgl],
            immediate_size: 0,
        });
        let resolve_color_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FluidSim ResolveColor Pipeline"),
            layout: Some(&resolve_color_layout),
            module: &scatter_shader,
            entry_point: Some("resolve_color_main"),
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
                // binding 1: vector field texture (textureLoad, no sampler)
                bgl_texture_unfilterable(1, wgpu::ShaderStages::COMPUTE),
                // binding 2: density texture (textureLoad, no sampler)
                bgl_texture_unfilterable(2, wgpu::ShaderStages::COMPUTE),
                // binding 3: uniforms
                bgl_uniform(3, wgpu::ShaderStages::COMPUTE),
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
                bgl_texture_filterable(3, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(4, wgpu::ShaderStages::FRAGMENT),
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

        // ── Uniform buffers ──
        let splat_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SplatUniforms>(), "FluidSim Splat Uniforms");
        let resolve_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ResolveUniforms>(), "FluidSim Resolve Uniforms");
        let splat_color_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SplatColorUniforms>(), "FluidSim SplatColor Uniforms");
        let resolve_color_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ResolveColorUniforms>(), "FluidSim ResolveColor Uniforms");
        let blur_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<BlurUniforms>(), "FluidSim Blur Uniforms");
        let gradient_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<GradientUniforms>(), "FluidSim Gradient Uniforms");
        let sim_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SimUniforms>(), "FluidSim Simulate Uniforms");
        let seed_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SeedUniforms>(), "FluidSim Seed Uniforms");
        let display_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<DisplayUniforms>(), "FluidSim Display Uniforms");

        // ── White texture fallback for display pass (1×1 RGBA white) ──
        let white_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("FluidSim White Fallback"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DENSITY_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let white_view = white_texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            splat_pipeline,
            splat_bgl,
            resolve_pipeline,
            resolve_bgl,
            splat_color_pipeline,
            splat_color_bgl,
            resolve_color_pipeline,
            resolve_color_bgl,
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
            particle_buffer: None,
            scatter_accum: None,
            color_accum: None,
            density_rt: None,
            blur_density_rt: None,
            vector_field_rt: None,
            blur_temp_rt: None,
            color_density_rt: None,
            splat_uniform_buf,
            resolve_uniform_buf,
            splat_color_uniform_buf,
            resolve_color_uniform_buf,
            blur_uniform_buf,
            gradient_uniform_buf,
            sim_uniform_buf,
            seed_uniform_buf,
            display_uniform_buf,
            white_texture,
            white_view,
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
            inject_zone_index: -1,
            inject_frames_remaining: 0,
            next_inject_zone: 0,
        }
    }

    fn init_resources(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_width: u32,
        output_height: u32,
        active_count: u32,
        density_res: f32,
    ) {
        self.active_count = active_count;
        self.current_density_res = density_res;

        // Scatter resolution = output * density_res (Unity: currentDensityRes drives InternalResolutionScale)
        let field_scale = density_res.clamp(0.125, 1.0);
        let sw = ((output_width as f32 * field_scale) as u32).max(64);
        let sh = ((output_height as f32 * field_scale) as u32).max(64);
        self.scatter_width = sw;
        self.scatter_height = sh;

        // Particle buffer: 8M × 48 bytes max, but we allocate only active_count
        let particle_buf_size = active_count as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Particle Buffer"),
            size: particle_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Scatter accum: sw × sh × 4 bytes (atomic u32, scalar density)
        let accum_size = (sw as u64) * (sh as u64) * 4;
        let scatter_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Scatter Accum"),
            size: accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&scatter_accum, 0, &vec![0u8; accum_size as usize]);

        // Color accum: sw × sh × 4 channels × 4 bytes (4 atomic u32 per texel)
        let color_accum_size = (sw as u64) * (sh as u64) * 4 * 4;
        let color_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Color Accum"),
            size: color_accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&color_accum, 0, &vec![0u8; color_accum_size as usize]);

        // Density RT: full scatter resolution
        let density_rt = RenderTarget::new(device, sw, sh, DENSITY_FORMAT, "FluidSim Density");

        // Blur + vector field RTs: half scatter resolution (Unity: PRE_SHRINK=2)
        let bw = (sw / PRE_SHRINK).max(1);
        let bh = (sh / PRE_SHRINK).max(1);
        let blur_density_rt = RenderTarget::new(device, bw, bh, DENSITY_FORMAT, "FluidSim Blur Density");
        let vector_field_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Vector Field");
        let blur_temp_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Blur Temp");

        // Color density RT: same resolution as scatter (bilinear sampling in display)
        let color_density_rt = RenderTarget::new(device, sw, sh, DENSITY_FORMAT, "FluidSim Color Density");

        self.particle_buffer = Some(particle_buffer);
        self.scatter_accum = Some(scatter_accum);
        self.color_accum = Some(color_accum);
        self.density_rt = Some(density_rt);
        self.blur_density_rt = Some(blur_density_rt);
        self.vector_field_rt = Some(vector_field_rt);
        self.blur_temp_rt = Some(blur_temp_rt);
        self.color_density_rt = Some(color_density_rt);
        self.frame_count = 0;
        self.initialized = true;
    }

    fn dispatch_seed(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        pattern: u32,
        trigger_count: u32,
    ) {
        let uniforms = SeedUniforms {
            active_count: self.active_count,
            pattern_index: pattern,
            trigger_count,
            _pad0: 0,
        };
        queue.write_buffer(&self.seed_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
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

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("FluidSim Seed Pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.seed_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups((self.active_count + 255) / 256, 1, 1);
    }

    fn run_blur_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        direction: [f32; 2],
        radius: f32,
        texel_x: f32,
        texel_y: f32,
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
        queue.write_buffer(&self.blur_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Blur BG"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.blur_uniform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FluidSim Blur Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

impl Generator for FluidSimulationGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::FluidSimulation
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
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

        // Metal max_storage_buffer_binding_size is 128MB. 2M × 48 bytes = 96MB (safe).
        let desired_count = ((particles_param * 1_000_000.0) as u32).clamp(100_000, 2_000_000);

        // --- Dynamic density resolution ---
        // Unity: if densityRes != currentDensityRes -> Resize(rt.width, rt.height)
        let needs_reinit = !self.initialized
            || desired_count != self.active_count
            || (density_res - self.current_density_res).abs() > 0.001;

        if needs_reinit {
            self.init_resources(device, queue, ctx.width, ctx.height, desired_count, density_res);
        }

        let sw = self.scatter_width;
        let sh = self.scatter_height;
        let active_count = self.active_count;
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
                    self.dispatch_seed(queue, encoder, device, pattern, trigger_count as u32);
                } else if self.active_snap_mode == 4 {
                    // Mode 4: inject zone (only when color mode is active)
                    if color_mode > 0 {
                        self.inject_zone_index = self.next_inject_zone;
                        self.inject_frames_remaining = INJECT_FRAMES_PER_ZONE;
                        self.next_inject_zone = (self.next_inject_zone + 1) % 4;
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
        // Unity: if colorMode == 0 && lastColorMode > 0 -> reset inject state
        if color_mode == 0 && self.last_color_mode > 0 {
            self.inject_zone_index = -1;
            self.inject_frames_remaining = 0;
            self.next_inject_zone = 0;
        }
        self.last_color_mode = color_mode;

        // --- Advance injection state machine ---
        // Unity: injectFramesRemaining--; if <= 0 -> injectZoneIndex = -1
        if self.inject_zone_index >= 0 {
            self.inject_frames_remaining -= 1;
            if self.inject_frames_remaining <= 0 {
                self.inject_zone_index = -1;
            }
        }

        let inject_phase = if self.inject_zone_index >= 0 {
            1.0 - (self.inject_frames_remaining as f32 / INJECT_FRAMES_PER_ZONE as f32)
        } else {
            0.0
        };
        let active_inject_force = if self.inject_zone_index >= 0 { inject_force } else { 0.0 };

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
        };
        queue.write_buffer(&self.splat_uniform_buf, 0, bytemuck::bytes_of(&splat_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let scatter_accum = self.scatter_accum.as_ref().unwrap();

        let splat_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Splat BG"),
            layout: &self.splat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.splat_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Splat Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.splat_pipeline);
            pass.set_bind_group(0, &splat_bg, &[]);
            pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
        }

        // Resolve accumulator to density texture
        let resolve_uniforms = ResolveUniforms { width: sw, height: sh, _pad0: 0, _pad1: 0 };
        queue.write_buffer(&self.resolve_uniform_buf, 0, bytemuck::bytes_of(&resolve_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();
        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Resolve BG"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: self.resolve_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Resolve Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.dispatch_workgroups((sw + 15) / 16, (sh + 15) / 16, 1);
        }

        // ================================================================
        // PHASE 1B: Color scatter (parallel to scalar, only when color_mode > 0)
        // Unity: DispatchColorScatter — SplatColorKernel + ResolveColorKernel
        // ================================================================

        if color_mode > 0 {
            let splat_color_uniforms = SplatColorUniforms {
                active_count,
                width: sw,
                height: sh,
                scaled_energy,
                color_mode: color_mode as u32,
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
            };
            queue.write_buffer(&self.splat_color_uniform_buf, 0, bytemuck::bytes_of(&splat_color_uniforms));

            let color_accum = self.color_accum.as_ref().unwrap();
            let splat_color_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim SplatColor BG"),
                layout: &self.splat_color_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: color_accum.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.splat_color_uniform_buf.as_entire_binding() },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim SplatColor Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.splat_color_pipeline);
                pass.set_bind_group(0, &splat_color_bg, &[]);
                pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
            }

            let resolve_color_uniforms = ResolveColorUniforms { width: sw, height: sh, _pad0: 0, _pad1: 0 };
            queue.write_buffer(&self.resolve_color_uniform_buf, 0, bytemuck::bytes_of(&resolve_color_uniforms));

            let color_density_rt = self.color_density_rt.as_ref().unwrap();
            let resolve_color_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim ResolveColor BG"),
                layout: &self.resolve_color_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: color_accum.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&color_density_rt.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: self.resolve_color_uniform_buf.as_entire_binding() },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim ResolveColor Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.resolve_color_pipeline);
                pass.set_bind_group(0, &resolve_color_bg, &[]);
                pass.dispatch_workgroups((sw + 15) / 16, (sh + 15) / 16, 1);
            }
        }

        // ================================================================
        // PHASE 2: Vector Field Generation
        // Blur 1 (H+V) → Gradient + Rotate → Blur 2 (H+V)
        // ================================================================

        let blur_radius = blur_radius_param.max(2.0);
        // Resolution-scaled blur radius (Unity: resScale = blurResW / 640.0)
        let res_scale = bw as f32 / 640.0;
        let scaled_radius = (blur_radius * res_scale).max(1.0);

        // Blur 1: density_rt → blur_density_rt (H), blur_density_rt → density_rt (V)
        // Unity: Blit source → blurredDensityRT, then ApplyGaussianBlur in-place
        let density_rt = self.density_rt.as_ref().unwrap();
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        self.run_blur_pass(device, queue, encoder, &density_rt.view, &blur_density_rt.view,
            &self.blur_pipeline, [1.0, 0.0], scaled_radius, 1.0 / sw as f32, 1.0 / sh as f32);
        let density_rt = self.density_rt.as_ref().unwrap();
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        self.run_blur_pass(device, queue, encoder, &blur_density_rt.view, &density_rt.view,
            &self.blur_pipeline, [0.0, 1.0], scaled_radius, 1.0 / bw as f32, 1.0 / bh as f32);

        // Gradient + Rotate: blurred density → vector field
        // Resolution-independent slope: slopeStrength * densityAreaScale
        // Unity: densityAreaScale = (trailWidth * trailHeight) / SCATTER_REFERENCE_AREA
        let density_area_scale = (sw as f32 * sh as f32) / SCATTER_REFERENCE_AREA;
        let rot_rad = rotation_deg_snap * std::f32::consts::PI / 180.0;

        let gradient_uniforms = GradientUniforms {
            texel_x: 1.0 / bw as f32,
            texel_y: 1.0 / bh as f32,
            slope_strength: slope_snap * density_area_scale,
            rot_cos: rot_rad.cos(),
            rot_sin: rot_rad.sin(),
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&self.gradient_uniform_buf, 0, bytemuck::bytes_of(&gradient_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let gradient_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim GradientRotate BG"),
            layout: &self.gradient_rotate_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.gradient_uniform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FluidSim GradientRotate Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &vector_field_rt.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.gradient_rotate_pipeline);
            pass.set_bind_group(0, &gradient_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Blur 2: vector_field (H+V) in-place via blur_temp
        let vector_texel_x = 1.0 / bw as f32;
        let vector_texel_y = 1.0 / bh as f32;
        let vector_blur_radius = scaled_radius * 0.5;

        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(device, queue, encoder, &vector_field_rt.view, &blur_temp_rt.view,
            &self.blur_vector_pipeline, [1.0, 0.0], vector_blur_radius, vector_texel_x, vector_texel_y);
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass(device, queue, encoder, &blur_temp_rt.view, &vector_field_rt.view,
            &self.blur_vector_pipeline, [0.0, 1.0], vector_blur_radius, vector_texel_x, vector_texel_y);

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
            inject_index: self.inject_zone_index,
            inject_force: active_inject_force,
            inject_phase,
            time_val: ctx.time,
            _pad: 0.0,
        };
        queue.write_buffer(&self.sim_uniform_buf, 0, bytemuck::bytes_of(&sim_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let density_rt = self.density_rt.as_ref().unwrap();

        let sim_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Simulate BG"),
            layout: &self.simulate_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vector_field_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 3, resource: self.sim_uniform_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim Simulate Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.simulate_pipeline);
            pass.set_bind_group(0, &sim_bg, &[]);
            pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
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
        queue.write_buffer(&self.display_uniform_buf, 0, bytemuck::bytes_of(&display_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();

        // Color texture: use color_density_rt when color_mode > 0, else white fallback
        let color_view: &wgpu::TextureView = if color_mode > 0 {
            &self.color_density_rt.as_ref().unwrap().view
        } else {
            &self.white_view
        };

        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.display_uniform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(color_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FluidSim Display Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
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
        // Force reinit on next render (safe: DefaultDecay=0, no accumulated state)
        self.initialized = false;
    }
}

// ── Helpers ──

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
