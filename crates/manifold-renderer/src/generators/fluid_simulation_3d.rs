// Volumetric 3D particle compute fluid simulation.
// 7 passes per frame (steps 1-4 amortized to alternate frames):
//   [alternate frames:]
//   3D Scatter (2 compute) -> 3D Blur Density (3x2 compute) -> GradientCurl3D (compute)
//   -> 3D Blur VectorField (3x2 compute)
//   [every frame:]
//   Simulate3D (compute) -> ProjectedScatter (2 compute) -> Display (fragment)

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::render_target::RenderTarget;
use super::compute_common::Particle;

// Parameter indices matching types.rs param_defs (26 params)
const FLOW: usize = 0;
const FEATHER: usize = 1;
const CURL: usize = 2;
const TURBULENCE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
const INVERT: usize = 6;
const SCALE: usize = 7;
const PARTICLES: usize = 8;
const SNAP: usize = 9;
const SNAP_MODE: usize = 10;
const PARTICLE_SIZE: usize = 11;
const FIELD_RES: usize = 12;
const ANTI_CLUMP: usize = 13;
const WANDER: usize = 14;
const RESPAWN: usize = 15;
const DENSE_RESPAWN: usize = 16;
const COLOR: usize = 17;
const COLOR_BRIGHT: usize = 18;
// ZONE_FORCE (19) unused
const CONTAINER: usize = 20;
const CTR_SCALE: usize = 21;
const VOL_RES: usize = 22;
const CAM_DIST: usize = 23;
const CAM_TILT: usize = 24;
const FLATTEN: usize = 25;

const DENSITY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const DENSITY_3D_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const VECTOR_3D_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

fn vol_res_from_param(val: f32) -> u32 {
    let idx = (val + 0.5) as u32;
    match idx {
        0 => 64,
        1 => 128,
        _ => 256,
    }
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat3DUniforms {
    active_count: u32,
    vol_res: u32,
    base_energy: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Resolve3DUniforms {
    vol_res: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blur3DUniforms {
    vol_res: u32,
    axis: u32,
    radius: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientCurl3DUniforms {
    vol_res: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    flow: f32,
    curl_angle: f32,
    time_val: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Sim3DUniforms {
    active_count: u32,
    vol_res: u32,
    frame_count: u32,
    container: f32,
    ctr_scale: f32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    wander: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    dt: f32,
    flatten: f32,
    cam_tilt: f32,
    cam_dist: f32,
    time_speed: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    container: f32,
    cam_dist: f32,
    cam_tilt: f32,
    time_speed: f32,
    aspect: f32,
    base_energy: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveDisplayUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count: u32,
    pattern_index: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    color_mode: f32,
    color_bright: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct FluidSimulation3DGenerator {
    // Compute pipelines — 3D volume
    splat_3d_pipeline: wgpu::ComputePipeline,
    splat_3d_bgl: wgpu::BindGroupLayout,
    resolve_3d_pipeline: wgpu::ComputePipeline,
    resolve_3d_bgl: wgpu::BindGroupLayout,
    blur_scalar_pipeline: wgpu::ComputePipeline,
    blur_scalar_bgl: wgpu::BindGroupLayout,
    blur_vector_pipeline: wgpu::ComputePipeline,
    blur_vector_bgl: wgpu::BindGroupLayout,
    gradient_curl_3d_pipeline: wgpu::ComputePipeline,
    gradient_curl_3d_bgl: wgpu::BindGroupLayout,

    // Compute pipelines — simulation + projected scatter
    simulate_3d_pipeline: wgpu::ComputePipeline,
    simulate_3d_bgl: wgpu::BindGroupLayout,
    splat_projected_pipeline: wgpu::ComputePipeline,
    splat_projected_bgl: wgpu::BindGroupLayout,
    resolve_display_pipeline: wgpu::ComputePipeline,
    resolve_display_bgl: wgpu::BindGroupLayout,

    // Seed pipeline (reuses 2D seed shader)
    seed_pipeline: wgpu::ComputePipeline,
    seed_bgl: wgpu::BindGroupLayout,

    // Display fragment pipeline (reuses 2D display shader)
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,

    // 2D blur pipelines (for projected density)
    blur_2d_pipeline: wgpu::RenderPipeline,
    blur_2d_bgl: wgpu::BindGroupLayout,

    // GPU resources (lazy-init)
    particle_buffer: Option<wgpu::Buffer>,
    accum_3d: Option<wgpu::Buffer>,
    display_accum: Option<wgpu::Buffer>,
    density_volume: Option<Volume3D>,
    density_blur_temp: Option<Volume3D>,
    vector_volume: Option<Volume3D>,
    vector_blur_temp: Option<Volume3D>,
    display_density_rt: Option<RenderTarget>,
    blur_display_rt: Option<RenderTarget>,

    // Uniform buffers
    splat_3d_uniform_buf: wgpu::Buffer,
    resolve_3d_uniform_buf: wgpu::Buffer,
    blur_3d_uniform_buf: wgpu::Buffer,
    gradient_curl_3d_uniform_buf: wgpu::Buffer,
    sim_3d_uniform_buf: wgpu::Buffer,
    projected_uniform_buf: wgpu::Buffer,
    resolve_display_uniform_buf: wgpu::Buffer,
    seed_uniform_buf: wgpu::Buffer,
    display_uniform_buf: wgpu::Buffer,
    blur_2d_uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    sampler_3d: wgpu::Sampler,

    // State
    active_count: u32,
    vol_res: u32,
    disp_w: u32,
    disp_h: u32,
    frame_count: u32,
    initialized: bool,
    needs_reseed: bool,
    prev_snap: f32,
}

/// 3D texture with view, for volume storage.
struct Volume3D {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    _res: u32,
}

impl Volume3D {
    fn new(device: &wgpu::Device, res: u32, format: wgpu::TextureFormat, label: &str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: res,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { _texture: texture, view, _res: res }
    }
}

impl FluidSimulation3DGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim3D Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        let sampler_3d = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim3D 3D Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        // ── Scatter 3D shader (4 entry points) ──
        let scatter_3d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Scatter3D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_scatter_3d.wgsl").into(),
            ),
        });

        // Splat 3D pipeline
        let splat_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Splat3D BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_3d_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &splat_3d_bgl, "splat_3d", "FluidSim3D Splat3D");

        // Resolve 3D pipeline
        let resolve_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Resolve3D BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(1, wgpu::ShaderStages::COMPUTE, DENSITY_3D_FORMAT),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let resolve_3d_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &resolve_3d_bgl, "resolve_3d", "FluidSim3D Resolve3D");

        // Splat projected pipeline
        let splat_projected_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D SplatProjected BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_projected_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &splat_projected_bgl, "splat_projected", "FluidSim3D SplatProjected");

        // Resolve display pipeline
        let resolve_display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D ResolveDisplay BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_2d(1, wgpu::ShaderStages::COMPUTE, DENSITY_FORMAT),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let resolve_display_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &resolve_display_bgl, "resolve_display", "FluidSim3D ResolveDisplay");

        // ── Blur 3D shader (2 entry points) ──
        let blur_3d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Blur3D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_blur_3d.wgsl").into(),
            ),
        });

        // Blur scalar pipeline (R16Float)
        let blur_scalar_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D BlurScalar BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, DENSITY_3D_FORMAT),
            ],
        });
        let blur_scalar_pipeline = create_compute_pipeline(device, &blur_3d_shader, &blur_scalar_bgl, "blur_scalar", "FluidSim3D BlurScalar");

        // Blur vector pipeline (Rgba16Float)
        let blur_vector_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D BlurVector BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, VECTOR_3D_FORMAT),
            ],
        });
        let blur_vector_pipeline = create_compute_pipeline(device, &blur_3d_shader, &blur_vector_bgl, "blur_vector", "FluidSim3D BlurVector");

        // ── Gradient+Curl 3D pipeline ──
        let gradient_curl_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D GradientCurl3D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_gradient_curl_3d.wgsl").into(),
            ),
        });

        let gradient_curl_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D GradientCurl3D BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, VECTOR_3D_FORMAT),
            ],
        });
        let gradient_curl_3d_pipeline = create_compute_pipeline(device, &gradient_curl_shader, &gradient_curl_3d_bgl, "main", "FluidSim3D GradientCurl3D");

        // ── Simulate 3D pipeline ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Simulate3D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_simulate_3d.wgsl").into(),
            ),
        });

        let simulate_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Simulate3D BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d_filterable(1, wgpu::ShaderStages::COMPUTE), // vector field (Rgba16Float, filterable)
                bgl_sampler(2, wgpu::ShaderStages::COMPUTE),              // filtering sampler (for vector field)
                bgl_texture_3d(3, wgpu::ShaderStages::COMPUTE),           // density (R32Float, uses textureLoad)
                bgl_uniform(4, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let simulate_3d_pipeline = create_compute_pipeline(device, &sim_shader, &simulate_3d_bgl, "main", "FluidSim3D Simulate3D");

        // ── Seed pipeline (reuse 2D seed shader) ──
        let seed_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Seed Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_seed.wgsl").into(),
            ),
        });

        let seed_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Seed BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(1, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let seed_pipeline = create_compute_pipeline(device, &seed_shader, &seed_bgl, "main", "FluidSim3D Seed");

        // ── Display fragment pipeline (reuse 2D display shader) ──
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Display Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_display.wgsl").into(),
            ),
        });

        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Display BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim3D Display Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = create_fragment_pipeline(
            device,
            &display_shader,
            &display_layout,
            target_format,
            "FluidSim3D Display",
        );

        // ── 2D Blur fragment pipeline (for projected density) ──
        let blur_2d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Blur2D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/gaussian_blur.wgsl").into(),
            ),
        });

        let blur_2d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Blur2D BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let blur_2d_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim3D Blur2D Layout"),
            bind_group_layouts: &[&blur_2d_bgl],
            immediate_size: 0,
        });

        let blur_2d_pipeline = create_fragment_pipeline(
            device,
            &blur_2d_shader,
            &blur_2d_layout,
            DENSITY_FORMAT,
            "FluidSim3D Blur2D (Density)",
        );

        // ── Uniform buffers ──
        let splat_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<Splat3DUniforms>(), "FluidSim3D Splat3D Uniforms");
        let resolve_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<Resolve3DUniforms>(), "FluidSim3D Resolve3D Uniforms");
        let blur_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<Blur3DUniforms>(), "FluidSim3D Blur3D Uniforms");
        let gradient_curl_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<GradientCurl3DUniforms>(), "FluidSim3D GradientCurl3D Uniforms");
        let sim_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<Sim3DUniforms>(), "FluidSim3D Sim3D Uniforms");
        let projected_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ProjectedUniforms>(), "FluidSim3D Projected Uniforms");
        let resolve_display_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ResolveDisplayUniforms>(), "FluidSim3D ResolveDisplay Uniforms");
        let seed_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SeedUniforms>(), "FluidSim3D Seed Uniforms");
        let display_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<DisplayUniforms>(), "FluidSim3D Display Uniforms");
        // BlurUniforms: 2 floats (direction) + radius + texel_x + texel_y + 3 pads = 32 bytes
        let blur_2d_uniform_buf = create_uniform_buffer(device, 32, "FluidSim3D Blur2D Uniforms");

        Self {
            splat_3d_pipeline,
            splat_3d_bgl,
            resolve_3d_pipeline,
            resolve_3d_bgl,
            blur_scalar_pipeline,
            blur_scalar_bgl,
            blur_vector_pipeline,
            blur_vector_bgl,
            gradient_curl_3d_pipeline,
            gradient_curl_3d_bgl,
            simulate_3d_pipeline,
            simulate_3d_bgl,
            splat_projected_pipeline,
            splat_projected_bgl,
            resolve_display_pipeline,
            resolve_display_bgl,
            seed_pipeline,
            seed_bgl,
            display_pipeline,
            display_bgl,
            blur_2d_pipeline,
            blur_2d_bgl,
            particle_buffer: None,
            accum_3d: None,
            display_accum: None,
            density_volume: None,
            density_blur_temp: None,
            vector_volume: None,
            vector_blur_temp: None,
            display_density_rt: None,
            blur_display_rt: None,
            splat_3d_uniform_buf,
            resolve_3d_uniform_buf,
            blur_3d_uniform_buf,
            gradient_curl_3d_uniform_buf,
            sim_3d_uniform_buf,
            projected_uniform_buf,
            resolve_display_uniform_buf,
            seed_uniform_buf,
            display_uniform_buf,
            blur_2d_uniform_buf,
            sampler,
            sampler_3d,
            active_count: 0,
            vol_res: 0,
            disp_w: 0,
            disp_h: 0,
            frame_count: 0,
            initialized: false,
            needs_reseed: true,
            prev_snap: 0.0,
        }
    }

    fn init_resources(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_width: u32,
        output_height: u32,
        active_count: u32,
        vol_res: u32,
        field_res: f32,
    ) {
        self.active_count = active_count;
        self.vol_res = vol_res;

        // Display resolution = output * field_res
        let field_scale = field_res.max(0.1);
        let dw = ((output_width as f32 * field_scale) as u32).max(64);
        let dh = ((output_height as f32 * field_scale) as u32).max(64);
        self.disp_w = dw;
        self.disp_h = dh;

        // Particle buffer
        let particle_buf_size = active_count as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim3D Particle Buffer"),
            size: particle_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 3D accumulator: vol_res^3 * 4 bytes
        let accum_3d_size = (vol_res as u64) * (vol_res as u64) * (vol_res as u64) * 4;
        let accum_3d = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim3D Accum3D"),
            size: accum_3d_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let zeros_3d = vec![0u8; accum_3d_size as usize];
        queue.write_buffer(&accum_3d, 0, &zeros_3d);

        // 2D display accumulator: dw * dh * 4 bytes
        let display_accum_size = (dw as u64) * (dh as u64) * 4;
        let display_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim3D DisplayAccum"),
            size: display_accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let zeros_disp = vec![0u8; display_accum_size as usize];
        queue.write_buffer(&display_accum, 0, &zeros_disp);

        // 3D volumes
        let density_volume = Volume3D::new(device, vol_res, DENSITY_3D_FORMAT, "FluidSim3D DensityVolume");
        let density_blur_temp = Volume3D::new(device, vol_res, DENSITY_3D_FORMAT, "FluidSim3D DensityBlurTemp");
        let vector_volume = Volume3D::new(device, vol_res, VECTOR_3D_FORMAT, "FluidSim3D VectorVolume");
        let vector_blur_temp = Volume3D::new(device, vol_res, VECTOR_3D_FORMAT, "FluidSim3D VectorBlurTemp");

        // 2D display density RT
        let display_density_rt = RenderTarget::new(device, dw, dh, DENSITY_FORMAT, "FluidSim3D DisplayDensity");
        let blur_display_rt = RenderTarget::new(device, dw, dh, DENSITY_FORMAT, "FluidSim3D BlurDisplay");

        self.particle_buffer = Some(particle_buffer);
        self.accum_3d = Some(accum_3d);
        self.display_accum = Some(display_accum);
        self.density_volume = Some(density_volume);
        self.density_blur_temp = Some(density_blur_temp);
        self.vector_volume = Some(vector_volume);
        self.vector_blur_temp = Some(vector_blur_temp);
        self.display_density_rt = Some(display_density_rt);
        self.blur_display_rt = Some(blur_display_rt);
        self.frame_count = 0;
        self.initialized = true;
        self.needs_reseed = true;
    }

    fn dispatch_seed(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        pattern: u32,
    ) {
        let uniforms = SeedUniforms {
            active_count: self.active_count,
            pattern_index: pattern,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&self.seed_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D Seed BG"),
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
            label: Some("FluidSim3D Seed Pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.seed_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups((self.active_count + 255) / 256, 1, 1);
    }

    fn dispatch_3d_blur_scalar(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        axis: u32,
        radius: f32,
        src_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
    ) {
        let uniforms = Blur3DUniforms {
            vol_res: self.vol_res,
            axis,
            radius,
            _pad: 0,
        };
        queue.write_buffer(&self.blur_3d_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D BlurScalar BG"),
            layout: &self.blur_scalar_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blur_3d_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(dst_view),
                },
            ],
        });

        let wg = (self.vol_res + 3) / 4;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("FluidSim3D BlurScalar Pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.blur_scalar_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(wg, wg, wg);
    }

    fn dispatch_3d_blur_vector(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        axis: u32,
        radius: f32,
        src_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
    ) {
        let uniforms = Blur3DUniforms {
            vol_res: self.vol_res,
            axis,
            radius,
            _pad: 0,
        };
        queue.write_buffer(&self.blur_3d_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D BlurVector BG"),
            layout: &self.blur_vector_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blur_3d_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(dst_view),
                },
            ],
        });

        let wg = (self.vol_res + 3) / 4;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("FluidSim3D BlurVector Pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.blur_vector_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(wg, wg, wg);
    }

    fn run_blur_2d_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        direction: [f32; 2],
        radius: f32,
        texel_x: f32,
        texel_y: f32,
    ) {
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

        let uniforms = BlurUniforms {
            direction,
            radius,
            texel_x,
            texel_y,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&self.blur_2d_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D Blur2D BG"),
            layout: &self.blur_2d_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blur_2d_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FluidSim3D Blur2D Pass"),
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
        pass.set_pipeline(&self.blur_2d_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

impl Generator for FluidSimulation3DGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::FluidSimulation3D
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        // Read all 26 params
        let flow = param(ctx, FLOW, -0.01);
        let feather = param(ctx, FEATHER, 20.0);
        let curl = param(ctx, CURL, 85.0);
        let turbulence = param(ctx, TURBULENCE, 0.001);
        let speed = param(ctx, SPEED, 1.0);
        let contrast = param(ctx, CONTRAST, 3.5);
        let invert = param(ctx, INVERT, 0.0);
        let _scale = param(ctx, SCALE, 1.0);
        let particles_param = param(ctx, PARTICLES, 2.0);
        let snap = param(ctx, SNAP, 0.0);
        let snap_mode = param(ctx, SNAP_MODE, 0.0);
        let particle_size = param(ctx, PARTICLE_SIZE, 3.0);
        let field_res = param(ctx, FIELD_RES, 0.5);
        let anti_clump = param(ctx, ANTI_CLUMP, 0.0);
        let wander = param(ctx, WANDER, 0.0);
        let respawn = param(ctx, RESPAWN, 0.001);
        let dense_respawn = param(ctx, DENSE_RESPAWN, 0.0);
        let color_mode = param(ctx, COLOR, 0.0);
        let color_bright = param(ctx, COLOR_BRIGHT, 2.0);
        let container = param(ctx, CONTAINER, 0.0);
        let ctr_scale = param(ctx, CTR_SCALE, 1.0);
        let vol_res_param = param(ctx, VOL_RES, 1.0);
        let cam_dist = param(ctx, CAM_DIST, 3.0);
        let cam_tilt = param(ctx, CAM_TILT, 0.3);
        let flatten = param(ctx, FLATTEN, 0.0);

        // Metal max_storage_buffer_binding_size is 128MB. 2M × 48 bytes = 96MB (safe).
        let desired_count = ((particles_param * 1_000_000.0) as u32).clamp(1000, 2_000_000);
        let desired_vol_res = vol_res_from_param(vol_res_param);

        // Lazy-init or reinit on particle count / volume resolution change
        if !self.initialized || desired_count != self.active_count || desired_vol_res != self.vol_res {
            self.init_resources(device, queue, ctx.width, ctx.height, desired_count, desired_vol_res, field_res);
        }

        let active_count = self.active_count;
        let vol_res = self.vol_res;
        let dw = self.disp_w;
        let dh = self.disp_h;

        // Snap trigger
        if snap > 0.5 && self.prev_snap <= 0.5 {
            self.needs_reseed = true;
        }
        self.prev_snap = snap;

        // ── Seed pass ──
        if self.needs_reseed {
            let pattern = snap_mode.round() as u32;
            self.dispatch_seed(queue, encoder, device, pattern);
            self.needs_reseed = false;
        }

        let time_speed = ctx.time * speed * 0.25;
        let blur_radius = feather.max(2.0);
        let do_volume_pass = self.frame_count % 2 == 0;

        // ══════════════════════════════════════════════════════════════════
        // ALTERNATE FRAMES: Volume pipeline (3D scatter, blur, gradient)
        // ══════════════════════════════════════════════════════════════════
        if do_volume_pass {
            // ── Pass 1a: Splat 3D ──
            let base_energy = 0.005 * (particle_size / 3.0) * (1000000.0 / active_count as f32);
            let splat_3d_uniforms = Splat3DUniforms {
                active_count,
                vol_res,
                base_energy,
                _pad: 0,
            };
            queue.write_buffer(&self.splat_3d_uniform_buf, 0, bytemuck::bytes_of(&splat_3d_uniforms));

            let particle_buffer = self.particle_buffer.as_ref().unwrap();
            let accum_3d = self.accum_3d.as_ref().unwrap();

            let splat_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim3D Splat3D BG"),
                layout: &self.splat_3d_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: particle_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: accum_3d.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.splat_3d_uniform_buf.as_entire_binding(),
                    },
                ],
            });

            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim3D Splat3D Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.splat_3d_pipeline);
                pass.set_bind_group(0, &splat_bg, &[]);
                pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
            }

            // ── Pass 1b: Resolve 3D ──
            let resolve_3d_uniforms = Resolve3DUniforms {
                vol_res,
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
            };
            queue.write_buffer(&self.resolve_3d_uniform_buf, 0, bytemuck::bytes_of(&resolve_3d_uniforms));

            let accum_3d = self.accum_3d.as_ref().unwrap();
            let density_vol = self.density_volume.as_ref().unwrap();

            let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim3D Resolve3D BG"),
                layout: &self.resolve_3d_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: accum_3d.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&density_vol.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.resolve_3d_uniform_buf.as_entire_binding(),
                    },
                ],
            });

            {
                let wg = (vol_res + 3) / 4;
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim3D Resolve3D Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.resolve_3d_pipeline);
                pass.set_bind_group(0, &resolve_bg, &[]);
                pass.dispatch_workgroups(wg, wg, wg);
            }

            // ── Pass 2: 3D Blur Density (X, Y, Z separable) ──
            let density_radius = blur_radius.min(vol_res as f32 / 4.0).max(1.0);
            {
                let density_vol = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // X: density_volume -> density_blur_temp
                self.dispatch_3d_blur_scalar(device, queue, encoder, 0, density_radius, &density_vol.view, &density_temp.view);
            }
            {
                let density_vol = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // Y: density_blur_temp -> density_volume
                self.dispatch_3d_blur_scalar(device, queue, encoder, 1, density_radius, &density_temp.view, &density_vol.view);
            }
            // Z pass: skip if vol_res depth < 8
            if vol_res >= 8 {
                let density_vol = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // Z: density_volume -> density_blur_temp
                self.dispatch_3d_blur_scalar(device, queue, encoder, 2, density_radius, &density_vol.view, &density_temp.view);
                // Copy result back: density_blur_temp -> density_volume (swap pointers conceptually)
                // For simplicity, do one more pass Z->density_volume. Actually, after Z blur the result
                // is in density_blur_temp. We need density_volume to have the final result.
                // Solution: do X->temp, Y->vol, Z->temp. Then gradient reads from temp.
                // But we already did X->temp, Y->vol. So Z: vol->temp. Gradient reads temp.
                // That's fine — we just need to use the right view for gradient.
            }

            // ── Pass 3: Gradient + Curl 3D ──
            let gradient_uniforms = GradientCurl3DUniforms {
                vol_res,
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
                flow,
                curl_angle: curl,
                time_val: ctx.time,
                _pad3: 0.0,
            };
            queue.write_buffer(&self.gradient_curl_3d_uniform_buf, 0, bytemuck::bytes_of(&gradient_uniforms));

            // After blur: if Z was done, blurred result is in density_blur_temp; else in density_volume
            let blurred_density_view = if vol_res >= 8 {
                &self.density_blur_temp.as_ref().unwrap().view
            } else {
                &self.density_volume.as_ref().unwrap().view
            };
            let vector_vol = self.vector_volume.as_ref().unwrap();

            let gradient_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FluidSim3D GradientCurl3D BG"),
                layout: &self.gradient_curl_3d_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.gradient_curl_3d_uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(blurred_density_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&vector_vol.view),
                    },
                ],
            });

            {
                let wg = (vol_res + 3) / 4;
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("FluidSim3D GradientCurl3D Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.gradient_curl_3d_pipeline);
                pass.set_bind_group(0, &gradient_bg, &[]);
                pass.dispatch_workgroups(wg, wg, wg);
            }

            // ── Pass 4: 3D Blur Vector Field (X, Y, Z separable) ──
            let vector_radius = (blur_radius * 0.5).min(vol_res as f32 / 4.0).max(1.0);
            {
                let vector_vol = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // X: vector_volume -> vector_blur_temp
                self.dispatch_3d_blur_vector(device, queue, encoder, 0, vector_radius, &vector_vol.view, &vector_temp.view);
            }
            {
                let vector_vol = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // Y: vector_blur_temp -> vector_volume
                self.dispatch_3d_blur_vector(device, queue, encoder, 1, vector_radius, &vector_temp.view, &vector_vol.view);
            }
            if vol_res >= 8 {
                let vector_vol = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // Z: vector_volume -> vector_blur_temp
                self.dispatch_3d_blur_vector(device, queue, encoder, 2, vector_radius, &vector_vol.view, &vector_temp.view);
                // After Z blur, result is in vector_blur_temp.
                // Simulate will read from the appropriate view below.
            }
        }

        // ══════════════════════════════════════════════════════════════════
        // EVERY FRAME: Simulate, Projected Scatter, Display
        // ══════════════════════════════════════════════════════════════════

        // ── Pass 5: Simulate 3D ──
        let sim_uniforms = Sim3DUniforms {
            active_count,
            vol_res,
            frame_count: self.frame_count,
            container,
            ctr_scale,
            speed,
            turbulence,
            anti_clump,
            wander,
            respawn_rate: respawn,
            dense_respawn,
            dt: ctx.dt,
            flatten,
            cam_tilt,
            cam_dist,
            time_speed,
        };
        queue.write_buffer(&self.sim_3d_uniform_buf, 0, bytemuck::bytes_of(&sim_uniforms));

        // Vector field source: if Z blur was done on this or previous volume frame, use blur temp
        let vector_field_view = if vol_res >= 8 {
            &self.vector_blur_temp.as_ref().unwrap().view
        } else {
            &self.vector_volume.as_ref().unwrap().view
        };

        // Density source for simulation: use the blurred density
        let density_view_for_sim = if vol_res >= 8 {
            &self.density_blur_temp.as_ref().unwrap().view
        } else {
            &self.density_volume.as_ref().unwrap().view
        };

        let particle_buffer = self.particle_buffer.as_ref().unwrap();

        let sim_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D Simulate3D BG"),
            layout: &self.simulate_3d_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particle_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(vector_field_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler_3d),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(density_view_for_sim),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.sim_3d_uniform_buf.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim3D Simulate3D Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.simulate_3d_pipeline);
            pass.set_bind_group(0, &sim_bg, &[]);
            pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
        }

        // ── Pass 6a: Splat Projected ──
        let proj_base_energy = 0.005 * (particle_size / 3.0) * (1000000.0 / active_count as f32);
        let proj_uniforms = ProjectedUniforms {
            active_count,
            disp_w: dw,
            disp_h: dh,
            container,
            cam_dist,
            cam_tilt,
            time_speed,
            aspect: ctx.aspect,
            base_energy: proj_base_energy,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&self.projected_uniform_buf, 0, bytemuck::bytes_of(&proj_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let display_accum = self.display_accum.as_ref().unwrap();

        let proj_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D SplatProjected BG"),
            layout: &self.splat_projected_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particle_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: display_accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.projected_uniform_buf.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim3D SplatProjected Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.splat_projected_pipeline);
            pass.set_bind_group(0, &proj_bg, &[]);
            pass.dispatch_workgroups((active_count + 255) / 256, 1, 1);
        }

        // ── Pass 6b: Resolve Display ──
        let resolve_disp_uniforms = ResolveDisplayUniforms {
            width: dw,
            height: dh,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&self.resolve_display_uniform_buf, 0, bytemuck::bytes_of(&resolve_disp_uniforms));

        let display_accum = self.display_accum.as_ref().unwrap();
        let display_density_rt = self.display_density_rt.as_ref().unwrap();

        let resolve_disp_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D ResolveDisplay BG"),
            layout: &self.resolve_display_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: display_accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&display_density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.resolve_display_uniform_buf.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FluidSim3D ResolveDisplay Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve_display_pipeline);
            pass.set_bind_group(0, &resolve_disp_bg, &[]);
            pass.dispatch_workgroups((dw + 15) / 16, (dh + 15) / 16, 1);
        }

        // ── Pass 6c: 2D Blur of projected density (H + V) ──
        let blur_2d_radius = (feather * 0.5).max(2.0);
        let texel_x = 1.0 / dw as f32;
        let texel_y = 1.0 / dh as f32;

        {
            let display_density_rt = self.display_density_rt.as_ref().unwrap();
            let blur_display_rt = self.blur_display_rt.as_ref().unwrap();
            // H blur: display_density -> blur_display
            self.run_blur_2d_pass(
                device, queue, encoder,
                &display_density_rt.view,
                &blur_display_rt.view,
                [1.0, 0.0],
                blur_2d_radius,
                texel_x,
                texel_y,
            );
        }
        {
            let display_density_rt = self.display_density_rt.as_ref().unwrap();
            let blur_display_rt = self.blur_display_rt.as_ref().unwrap();
            // V blur: blur_display -> display_density
            self.run_blur_2d_pass(
                device, queue, encoder,
                &blur_display_rt.view,
                &display_density_rt.view,
                [0.0, 1.0],
                blur_2d_radius,
                texel_x,
                texel_y,
            );
        }

        // ── Pass 7: Display — tone map density to target ──
        let trail_area = dw as f32 * dh as f32;
        let intensity = 3.0 * (trail_area / (1920.0 * 1080.0));

        let display_uniforms = DisplayUniforms {
            intensity,
            contrast,
            invert,
            color_mode,
            color_bright,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&self.display_uniform_buf, 0, bytemuck::bytes_of(&display_uniforms));

        let display_density_rt = self.display_density_rt.as_ref().unwrap();

        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.display_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&display_density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FluidSim3D Display Pass"),
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

fn bgl_texture_3d(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
        count: None,
    }
}

fn bgl_texture_3d_filterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
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

fn bgl_sampler(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn bgl_sampler_non_filtering(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
        count: None,
    }
}

fn bgl_storage_texture_3d(binding: u32, visibility: wgpu::ShaderStages, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D3,
        },
        count: None,
    }
}

fn bgl_storage_texture_2d(binding: u32, visibility: wgpu::ShaderStages, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
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

fn create_compute_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    bgl: &wgpu::BindGroupLayout,
    entry_point: &str,
    label: &str,
) -> wgpu::ComputePipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{label} Layout")),
        bind_group_layouts: &[bgl],
        immediate_size: 0,
    });

    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: shader,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
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
