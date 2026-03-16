// Density-displacement particle compute fluid simulation.
// 6 passes per frame:
//   Scatter (2 compute) -> Blur density (2 fragment) -> GradientRotate (fragment)
//   -> Blur vector (2 fragment) -> Simulate (compute) -> Display (fragment)

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::render_target::RenderTarget;
use super::compute_common::Particle;

// Parameter indices matching types.rs param_defs (20 params)
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
const ZONE_FORCE: usize = 19;

const DENSITY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const VECTOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    splat_size: f32,
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
    curl_angle_rad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    wander: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    dt: f32,
    frame_count: u32,
    _pad: f32,
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
    density_rt: Option<RenderTarget>,
    blur_density_rt: Option<RenderTarget>,
    vector_field_rt: Option<RenderTarget>,
    blur_temp_rt: Option<RenderTarget>,

    // Uniform buffers
    splat_uniform_buf: wgpu::Buffer,
    resolve_uniform_buf: wgpu::Buffer,
    blur_uniform_buf: wgpu::Buffer,
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
    needs_reseed: bool,
    prev_snap: f32,
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

        // ── Splat compute pipeline ──
        let splat_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim Splat Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_scatter.wgsl").into(),
            ),
        });

        let splat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Splat BGL"),
            entries: &[
                // binding 0: particles (read)
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                // binding 1: accum (read_write)
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                // binding 2: uniforms
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
            module: &splat_shader,
            entry_point: Some("splat_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Resolve compute pipeline (same shader, different entry point) ──
        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim Resolve BGL"),
            entries: &[
                // binding 0: accum (read_write)
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                // binding 1: density_out (storage texture write)
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
                // binding 2: uniforms
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
            module: &splat_shader,
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
                // binding 1: vector field texture (read)
                bgl_texture(1, wgpu::ShaderStages::COMPUTE),
                // binding 2: sampler
                bgl_sampler(2, wgpu::ShaderStages::COMPUTE),
                // binding 3: density texture (read)
                bgl_texture(3, wgpu::ShaderStages::COMPUTE),
                // binding 4: uniforms
                bgl_uniform(4, wgpu::ShaderStages::COMPUTE),
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

        // Blur pipeline renders to DENSITY_FORMAT for density blur;
        // for vector field blur we need VECTOR_FORMAT. Since both are used,
        // we create two render pipelines or use a single compatible format.
        // Use R32Float for density blur passes. Vector field blur uses Rg32Float
        // but the blur shader reads/writes the same channels — we need separate
        // pipelines for the two formats.
        // Actually: we'll create TWO blur pipelines (one per output format).
        // Store as blur_pipeline (R32Float) and blur_vector_pipeline (Rg32Float).
        // ... But the plan says single blur_pipeline. Let's use R32Float for
        // density and create the vector blur inline.
        //
        // Simpler approach: the gaussian_blur shader outputs vec4, so it works
        // with any format. We create two pipelines sharing the same BGL.
        let blur_pipeline = create_fragment_pipeline(
            device,
            &blur_shader,
            &blur_layout,
            DENSITY_FORMAT,
            "FluidSim Blur (Density)",
        );

        let blur_vector_pipeline = create_fragment_pipeline(
            device,
            &blur_shader,
            &blur_layout,
            VECTOR_FORMAT,
            "FluidSim Blur (Vector)",
        );

        // ── Gradient+Rotate fragment pipeline ──
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
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let gradient_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim GradientRotate Layout"),
            bind_group_layouts: &[&gradient_rotate_bgl],
            immediate_size: 0,
        });

        let gradient_rotate_pipeline = create_fragment_pipeline(
            device,
            &gradient_shader,
            &gradient_layout,
            VECTOR_FORMAT,
            "FluidSim GradientRotate",
        );

        // ── Display fragment pipeline ──
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
            device,
            &display_shader,
            &display_layout,
            target_format,
            "FluidSim Display",
        );

        // ── Uniform buffers ──
        let splat_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<SplatUniforms>(), "FluidSim Splat Uniforms");
        let resolve_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<ResolveUniforms>(), "FluidSim Resolve Uniforms");
        let blur_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<BlurUniforms>(), "FluidSim Blur Uniforms");
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
            particle_buffer: None,
            scatter_accum: None,
            density_rt: None,
            blur_density_rt: None,
            vector_field_rt: None,
            blur_temp_rt: None,
            splat_uniform_buf,
            resolve_uniform_buf,
            blur_uniform_buf,
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
        field_res: f32,
    ) {
        self.active_count = active_count;

        // Scatter resolution = output * field_res
        let field_scale = field_res.max(0.1);
        let sw = ((output_width as f32 * field_scale) as u32).max(64);
        let sh = ((output_height as f32 * field_scale) as u32).max(64);
        self.scatter_width = sw;
        self.scatter_height = sh;

        // Particle buffer
        let particle_buf_size = active_count as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Particle Buffer"),
            size: particle_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Scatter accumulator: sw * sh * 4 bytes (atomic u32)
        let accum_size = (sw as u64) * (sh as u64) * 4;
        let scatter_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim Scatter Accum"),
            size: accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let zeros = vec![0u8; accum_size as usize];
        queue.write_buffer(&scatter_accum, 0, &zeros);

        // Density RT: full scatter resolution
        let density_rt = RenderTarget::new(device, sw, sh, DENSITY_FORMAT, "FluidSim Density");

        // Blur density RT: half scatter resolution
        let bw = (sw / 2).max(1);
        let bh = (sh / 2).max(1);
        let blur_density_rt = RenderTarget::new(device, bw, bh, DENSITY_FORMAT, "FluidSim Blur Density");

        // Vector field RT: half scatter resolution
        let vector_field_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Vector Field");

        // Blur temp RT: same as vector field (for ping-pong blur)
        let blur_temp_rt = RenderTarget::new(device, bw, bh, VECTOR_FORMAT, "FluidSim Blur Temp");

        self.particle_buffer = Some(particle_buffer);
        self.scatter_accum = Some(scatter_accum);
        self.density_rt = Some(density_rt);
        self.blur_density_rt = Some(blur_density_rt);
        self.vector_field_rt = Some(vector_field_rt);
        self.blur_temp_rt = Some(blur_temp_rt);
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
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blur_uniform_buf.as_entire_binding(),
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
        pass.set_pipeline(&self.blur_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    fn run_blur_pass_vector(
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
            label: Some("FluidSim Blur Vector BG"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.blur_uniform_buf.as_entire_binding(),
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
            label: Some("FluidSim Blur Vector Pass"),
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
        pass.set_pipeline(&self.blur_vector_pipeline);
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
        let _zone_force = param(ctx, ZONE_FORCE, 0.0);

        let desired_count = ((particles_param * 1_000_000.0) as u32).clamp(1000, 4_000_000);

        // Lazy-init or reinit on particle count / resolution change
        if !self.initialized || desired_count != self.active_count {
            self.init_resources(device, queue, ctx.width, ctx.height, desired_count, field_res);
        }

        let sw = self.scatter_width;
        let sh = self.scatter_height;
        let active_count = self.active_count;
        let bw = (sw / 2).max(1);
        let bh = (sh / 2).max(1);

        // Snap trigger: transition from 0 to 1
        if snap > 0.5 && self.prev_snap <= 0.5 {
            self.needs_reseed = true;
        }
        self.prev_snap = snap;

        // ── Seed pass (once on init or snap) ──
        if self.needs_reseed {
            let pattern = snap_mode.round() as u32;
            self.dispatch_seed(queue, encoder, device, pattern);
            self.needs_reseed = false;
        }

        // ── Pass 1: Scatter — splat particles into accumulator ──
        let splat_uniforms = SplatUniforms {
            active_count,
            width: sw,
            height: sh,
            splat_size: particle_size,
        };
        queue.write_buffer(&self.splat_uniform_buf, 0, bytemuck::bytes_of(&splat_uniforms));

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let scatter_accum = self.scatter_accum.as_ref().unwrap();

        let splat_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Splat BG"),
            layout: &self.splat_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particle_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: scatter_accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.splat_uniform_buf.as_entire_binding(),
                },
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

        // ── Pass 2: Resolve accumulator to density texture ──
        let resolve_uniforms = ResolveUniforms {
            width: sw,
            height: sh,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&self.resolve_uniform_buf, 0, bytemuck::bytes_of(&resolve_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();

        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Resolve BG"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: scatter_accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.resolve_uniform_buf.as_entire_binding(),
                },
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

        // ── Pass 3: Blur density (H + V separable) ──
        // H blur: density_rt (full scatter res) → blur_density_rt (half, implicit downsample)
        // V blur: blur_density_rt (half) → density_rt (reused as blurred density output)
        // Gradient and simulate passes read from density_rt after this.
        let blur_radius = feather.max(2.0);
        let half_texel_x = 1.0 / bw as f32;
        let half_texel_y = 1.0 / bh as f32;

        let density_rt = self.density_rt.as_ref().unwrap();
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();

        self.run_blur_pass(
            device, queue, encoder,
            &density_rt.view,
            &blur_density_rt.view,
            [1.0, 0.0],
            blur_radius,
            1.0 / sw as f32,
            1.0 / sh as f32,
        );

        let density_rt = self.density_rt.as_ref().unwrap();
        let blur_density_rt = self.blur_density_rt.as_ref().unwrap();
        self.run_blur_pass(
            device, queue, encoder,
            &blur_density_rt.view,
            &density_rt.view,
            [0.0, 1.0],
            blur_radius,
            half_texel_x,
            half_texel_y,
        );

        // ── Pass 4: Gradient + Rotate → vector field ──
        let curl_angle_rad = curl * std::f32::consts::PI / 180.0;
        let slope_strength = flow * 500.0; // flow is negative → attraction

        let gradient_uniforms = GradientUniforms {
            texel_x: 1.0 / density_rt.width as f32,
            texel_y: 1.0 / density_rt.height as f32,
            slope_strength,
            curl_angle_rad,
        };
        queue.write_buffer(&self.gradient_uniform_buf, 0, bytemuck::bytes_of(&gradient_uniforms));

        let density_rt = self.density_rt.as_ref().unwrap();
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();

        let gradient_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim GradientRotate BG"),
            layout: &self.gradient_rotate_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.gradient_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
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

        // ── Pass 5: Blur vector field (H + V) ──
        let vector_texel_x = 1.0 / bw as f32;
        let vector_texel_y = 1.0 / bh as f32;
        let vector_blur_radius = feather.max(2.0) * 0.5;

        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();

        // H blur: vector_field → blur_temp
        self.run_blur_pass_vector(
            device, queue, encoder,
            &vector_field_rt.view,
            &blur_temp_rt.view,
            [1.0, 0.0],
            vector_blur_radius,
            vector_texel_x,
            vector_texel_y,
        );

        // V blur: blur_temp → vector_field
        let vector_field_rt = self.vector_field_rt.as_ref().unwrap();
        let blur_temp_rt = self.blur_temp_rt.as_ref().unwrap();
        self.run_blur_pass_vector(
            device, queue, encoder,
            &blur_temp_rt.view,
            &vector_field_rt.view,
            [0.0, 1.0],
            vector_blur_radius,
            vector_texel_x,
            vector_texel_y,
        );

        // ── Pass 6: Simulate — update particles ──
        let sim_uniforms = SimUniforms {
            active_count,
            field_width: bw,
            field_height: bh,
            speed,
            turbulence,
            anti_clump,
            wander,
            respawn_rate: respawn,
            dense_respawn,
            dt: ctx.dt,
            frame_count: self.frame_count,
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
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: particle_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&vector_field_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.sim_uniform_buf.as_entire_binding(),
                },
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

        // ── Pass 7: Display — tone map density to target ──
        let trail_area = sw as f32 * sh as f32;
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

        let density_rt = self.density_rt.as_ref().unwrap();

        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.display_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&density_rt.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
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

fn bgl_texture(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
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
