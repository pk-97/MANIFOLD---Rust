// Physarum agent-based compute pipeline: 500K agents with atomic scatter,
// trail diffusion via 3-pass fragment blur, and HSV display output.
//
// 4 passes per frame:
//   AgentUpdate (compute) → ResolveDeposit (compute) → DiffuseDecay (3× fragment) → Display (fragment)

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::render_target::RenderTarget;
use super::compute_common::{PhysarumAgent, FIXED_POINT_SCALE};

// Parameter indices matching types.rs param_defs
const SENS_DIST: usize = 0;
const SENS_ANGLE: usize = 1;
const TURN: usize = 2;
const STEP: usize = 3;
const DEPOSIT: usize = 4;
const DECAY: usize = 5;
const COLOR: usize = 6;
const GLOW: usize = 7;
const REACTIVITY: usize = 8;
const AGENTS: usize = 9;
const SCALE: usize = 10;
const SEEDS: usize = 11;

// Trail format: Rgba16Float (Unity: RFloat / R32Float, but R32Float is not filterable on Metal
// and trail textures need both STORAGE_BINDING and filtered sampling)
const TRAIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const MAX_AGENTS: u32 = 500_000;
const MIN_AGENTS: u32 = 10_000;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AgentUniforms {
    agent_count: u32,
    width: u32,
    height: u32,
    sensor_dist: f32,
    sensor_angle: f32,
    rotation_angle: f32,
    step_size: f32,
    deposit_scaled: f32,
    frame_count: u32,
    beat: f32,
    reactivity: f32,
    _pad: f32,
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
struct DiffuseUniforms {
    decay: f32,
    sub_decay: f32,
    texel_x: f32,
    texel_y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    hue: f32,
    glow: f32,
    uv_scale: f32,
    time: f32,
}

pub struct MyceliumGenerator {
    // Compute pipelines
    agent_update_pipeline: wgpu::ComputePipeline,
    agent_update_bgl: wgpu::BindGroupLayout,
    resolve_pipeline: wgpu::ComputePipeline,
    resolve_bgl: wgpu::BindGroupLayout,

    // Fragment pipelines
    diffuse_pipeline: wgpu::RenderPipeline,
    diffuse_bgl: wgpu::BindGroupLayout,
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,

    // GPU resources (lazy-init on first render)
    agent_buffer: Option<wgpu::Buffer>,
    accum_buffer: Option<wgpu::Buffer>,
    trail_a: Option<RenderTarget>,
    trail_b: Option<RenderTarget>,

    // Uniform buffers
    agent_uniform_buffer: wgpu::Buffer,
    resolve_uniform_buffer: wgpu::Buffer,
    diffuse_uniform_buffer: wgpu::Buffer,
    display_uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,

    // State
    agent_count: u32,
    trail_width: u32,
    trail_height: u32,
    frame_count: u32,
    initialized: bool,
    current_seeds: u32,
}

impl MyceliumGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Mycelium Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        // ── Agent Update compute pipeline ──
        let agent_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Agent Update Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mycelium_agent_update.wgsl").into(),
            ),
        });

        let agent_update_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Agent Update BGL"),
            entries: &[
                // binding 0: agents storage RW
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: trail texture (read, filterable)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 2: accum buffer (atomic RW)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 3: uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let agent_update_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mycelium Agent Update Layout"),
            bind_group_layouts: &[&agent_update_bgl],
            immediate_size: 0,
        });

        let agent_update_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Mycelium Agent Update Pipeline"),
            layout: Some(&agent_update_layout),
            module: &agent_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Resolve compute pipeline ──
        let resolve_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Resolve Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mycelium_resolve.wgsl").into(),
            ),
        });

        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Resolve BGL"),
            entries: &[
                // binding 0: trail_read (texture, filterable)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 1: trail_write (storage texture)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: TRAIL_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                // binding 2: accum buffer (atomic RW)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 3: uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let resolve_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mycelium Resolve Layout"),
            bind_group_layouts: &[&resolve_bgl],
            immediate_size: 0,
        });

        let resolve_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Mycelium Resolve Pipeline"),
            layout: Some(&resolve_layout),
            module: &resolve_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Diffuse fragment pipeline ──
        let diffuse_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Diffuse Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mycelium_diffuse.wgsl").into(),
            ),
        });

        let diffuse_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Diffuse BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let diffuse_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mycelium Diffuse Layout"),
            bind_group_layouts: &[&diffuse_bgl],
            immediate_size: 0,
        });

        let diffuse_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mycelium Diffuse Pipeline"),
            layout: Some(&diffuse_layout),
            vertex: wgpu::VertexState {
                module: &diffuse_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &diffuse_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TRAIL_FORMAT,
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
        });

        // ── Display fragment pipeline ──
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Display Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mycelium_display.wgsl").into(),
            ),
        });

        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Display BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mycelium Display Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mycelium Display Pipeline"),
            layout: Some(&display_layout),
            vertex: wgpu::VertexState {
                module: &display_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &display_shader,
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
        });

        let agent_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Agent Uniforms"),
            size: std::mem::size_of::<AgentUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let resolve_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Resolve Uniforms"),
            size: std::mem::size_of::<ResolveUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let diffuse_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Diffuse Uniforms"),
            size: std::mem::size_of::<DiffuseUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let display_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Display Uniforms"),
            size: std::mem::size_of::<DisplayUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            agent_update_pipeline,
            agent_update_bgl,
            resolve_pipeline,
            resolve_bgl,
            diffuse_pipeline,
            diffuse_bgl,
            display_pipeline,
            display_bgl,
            agent_buffer: None,
            accum_buffer: None,
            trail_a: None,
            trail_b: None,
            agent_uniform_buffer,
            resolve_uniform_buffer,
            diffuse_uniform_buffer,
            display_uniform_buffer,
            sampler,
            agent_count: 0,
            trail_width: 0,
            trail_height: 0,
            frame_count: 0,
            initialized: false,
            current_seeds: 0,
        }
    }

    fn init_resources(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32, agent_count: u32, seeds: u32) {
        let tw = (width / 2).max(1);
        let th = (height / 2).max(1);
        self.trail_width = tw;
        self.trail_height = th;
        self.agent_count = agent_count;
        self.current_seeds = seeds;
        self.frame_count = 0;

        // Agent buffer: agent_count * 16 bytes
        let agent_buf_size = (agent_count as u64) * (std::mem::size_of::<PhysarumAgent>() as u64);
        let agent_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Agent Buffer"),
            size: agent_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Seed agents with random positions
        self.seed_agents(queue, &agent_buffer, agent_count, seeds);

        // Accumulator buffer: tw * th * 4 bytes (atomic u32)
        let accum_size = (tw as u64) * (th as u64) * 4;
        let accum_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mycelium Accum Buffer"),
            size: accum_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Zero the accum buffer
        let zeros = vec![0u8; accum_size as usize];
        queue.write_buffer(&accum_buffer, 0, &zeros);

        // Trail textures (Rgba16Float, half-res)
        let trail_a = RenderTarget::new(device, tw, th, TRAIL_FORMAT, "Mycelium Trail A");
        let trail_b = RenderTarget::new(device, tw, th, TRAIL_FORMAT, "Mycelium Trail B");

        self.agent_buffer = Some(agent_buffer);
        self.accum_buffer = Some(accum_buffer);
        self.trail_a = Some(trail_a);
        self.trail_b = Some(trail_b);
        self.initialized = true;
    }

    fn seed_agents(&self, queue: &wgpu::Queue, buffer: &wgpu::Buffer, count: u32, seeds: u32) {
        let mut agents = Vec::with_capacity(count as usize);
        let seed_base = seeds.wrapping_mul(2654435761);
        for i in 0..count {
            let h = wang_hash_cpu(i.wrapping_add(seed_base));
            let h2 = wang_hash_cpu(h);
            let h3 = wang_hash_cpu(h2);
            let px = (h as f32) / 4294967296.0;
            let py = (h2 as f32) / 4294967296.0;
            let angle = (h3 as f32) / 4294967296.0 * std::f32::consts::TAU;
            agents.push(PhysarumAgent {
                pos: [px, py],
                angle,
                _pad: 0.0,
            });
        }
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&agents));
    }

    fn run_diffuse_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        decay: f32,
        sub_decay: f32,
    ) {
        let uniforms = DiffuseUniforms {
            decay,
            sub_decay,
            texel_x: 1.0 / self.trail_width as f32,
            texel_y: 1.0 / self.trail_height as f32,
        };
        queue.write_buffer(&self.diffuse_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Diffuse BG"),
            layout: &self.diffuse_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.diffuse_uniform_buffer.as_entire_binding(),
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

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mycelium Diffuse Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.diffuse_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn wang_hash_cpu(seed_in: u32) -> u32 {
    let mut seed = seed_in;
    seed = (seed ^ 61) ^ (seed >> 16);
    seed = seed.wrapping_mul(9);
    seed = seed ^ (seed >> 4);
    seed = seed.wrapping_mul(0x27d4eb2d);
    seed = seed ^ (seed >> 15);
    seed
}

impl Generator for MyceliumGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Mycelium
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let sens_dist = if ctx.param_count > SENS_DIST as u32 { ctx.params[SENS_DIST] } else { 0.02 };
        let sens_angle = if ctx.param_count > SENS_ANGLE as u32 { ctx.params[SENS_ANGLE] } else { 0.8 };
        let turn = if ctx.param_count > TURN as u32 { ctx.params[TURN] } else { 0.4 };
        let step = if ctx.param_count > STEP as u32 { ctx.params[STEP] } else { 0.001 };
        let deposit = if ctx.param_count > DEPOSIT as u32 { ctx.params[DEPOSIT] } else { 1.5 };
        let decay = if ctx.param_count > DECAY as u32 { ctx.params[DECAY] } else { 0.98 };
        let color_hue = if ctx.param_count > COLOR as u32 { ctx.params[COLOR] } else { 0.08 };
        let glow = if ctx.param_count > GLOW as u32 { ctx.params[GLOW] } else { 1.0 };
        let reactivity = if ctx.param_count > REACTIVITY as u32 { ctx.params[REACTIVITY] } else { 0.5 };
        let agents_param = if ctx.param_count > AGENTS as u32 { ctx.params[AGENTS] } else { 200.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let seeds = if ctx.param_count > SEEDS as u32 { ctx.params[SEEDS] } else { 1.0 };

        let desired_agents = ((agents_param * 1000.0) as u32).clamp(MIN_AGENTS, MAX_AGENTS);
        let seeds_int = seeds.to_bits();

        // Lazy-init or re-seed
        if !self.initialized || seeds_int != self.current_seeds || desired_agents != self.agent_count {
            self.init_resources(device, queue, ctx.width, ctx.height, desired_agents, seeds_int);
        }

        let tw = self.trail_width;
        let th = self.trail_height;
        let agent_count = self.agent_count;

        // ── Pass 1: Agent Update (compute) ──
        let deposit_scaled = (deposit * FIXED_POINT_SCALE * 0.01) as u32;
        let agent_uniforms = AgentUniforms {
            agent_count,
            width: tw,
            height: th,
            sensor_dist: sens_dist,
            sensor_angle: sens_angle,
            rotation_angle: turn,
            step_size: step,
            deposit_scaled: deposit_scaled as f32,
            frame_count: self.frame_count,
            beat: ctx.beat,
            reactivity,
            _pad: 0.0,
        };
        queue.write_buffer(&self.agent_uniform_buffer, 0, bytemuck::bytes_of(&agent_uniforms));

        let trail_a = self.trail_a.as_ref().unwrap();
        let trail_b = self.trail_b.as_ref().unwrap();
        let agent_buffer = self.agent_buffer.as_ref().unwrap();
        let accum_buffer = self.accum_buffer.as_ref().unwrap();

        // Agent update reads trail_a, writes to accum
        let agent_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Agent Update BG"),
            layout: &self.agent_update_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: agent_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&trail_a.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: accum_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.agent_uniform_buffer.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Mycelium Agent Update Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.agent_update_pipeline);
            pass.set_bind_group(0, &agent_bg, &[]);
            pass.dispatch_workgroups(agent_count.div_ceil(256), 1, 1);
        }

        // ── Pass 2: Resolve (compute) ──
        // Reads trail_a + accum → writes trail_b
        let resolve_uniforms = ResolveUniforms {
            width: tw,
            height: th,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&self.resolve_uniform_buffer, 0, bytemuck::bytes_of(&resolve_uniforms));

        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Resolve BG"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&trail_a.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&trail_b.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: accum_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.resolve_uniform_buffer.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Mycelium Resolve Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.dispatch_workgroups(tw.div_ceil(16), th.div_ceil(16), 1);
        }

        // ── Pass 3: Diffuse (3 fragment blits) ──
        // After resolve: trail_b has new data. trail_a is stale.
        // Pass 0: B→A with decay + evaporation
        let trail_a = self.trail_a.as_ref().unwrap();
        let trail_b = self.trail_b.as_ref().unwrap();
        self.run_diffuse_pass(device, queue, encoder, &trail_b.view, &trail_a.view, decay, 0.003);
        // Pass 1: A→B pure blur
        let trail_a = self.trail_a.as_ref().unwrap();
        let trail_b = self.trail_b.as_ref().unwrap();
        self.run_diffuse_pass(device, queue, encoder, &trail_a.view, &trail_b.view, 1.0, 0.0);
        // Pass 2: B→A pure blur
        let trail_a = self.trail_a.as_ref().unwrap();
        let trail_b = self.trail_b.as_ref().unwrap();
        self.run_diffuse_pass(device, queue, encoder, &trail_b.view, &trail_a.view, 1.0, 0.0);

        // ── Pass 4: Display (fragment) ──
        // trail_a has final diffused result
        let uv_scale = if scale > 0.0 { scale } else { 1.0 };
        let display_uniforms = DisplayUniforms {
            hue: color_hue,
            glow,
            uv_scale,
            time: ctx.time,
        };
        queue.write_buffer(&self.display_uniform_buffer, 0, bytemuck::bytes_of(&display_uniforms));

        let trail_a = self.trail_a.as_ref().unwrap();
        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.display_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&trail_a.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mycelium Display Pass"),
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
