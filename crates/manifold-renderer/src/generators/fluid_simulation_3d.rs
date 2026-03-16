// Simplified 3D fluid simulation via stateful ping-pong fragment shader.
// Same as FluidSimulation but with 3D camera projection params.
// Full volumetric compute pipeline deferred to later pass.

use manifold_core::GeneratorType;
use crate::blit::BlitPipeline;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::stateful_base::StatefulState;

// Parameter indices matching types.rs param_defs (26 params)
// Params 0-19: identical to FluidSimulation
const FLOW: usize = 0;
const FEATHER: usize = 1;
const CURL: usize = 2;
const TURBULENCE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
const INVERT: usize = 6;
const SCALE: usize = 7;
// PARTICLES (8), SNAP (9), SNAP_MODE (10), PARTICLE_SIZE (11),
// FIELD_RES (12), ANTI_CLUMP (13), WANDER (14), RESPAWN (15),
// DENSE_RESPAWN (16) — not used in simplified version
const COLOR: usize = 17;
const COLOR_BRIGHT: usize = 18;
// ZONE_FORCE (19) — not used in simplified version
// Params 20-25: 3D-specific
const CONTAINER: usize = 20;
// CTR_SCALE (21), VOL_RES (22) — not used in simplified version
const CAM_DIST: usize = 23;
const CAM_TILT: usize = 24;
const FLATTEN: usize = 25;

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Fluid3DUniforms {
    time_val: f32,
    flow: f32,
    feather: f32,
    curl_angle: f32,
    turbulence: f32,
    speed: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
    texel_x: f32,
    texel_y: f32,
    color_mode: f32,
    color_bright: f32,
    decay: f32,
    cam_dist: f32,
    cam_tilt: f32,
    flatten: f32,
    container: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct FluidSimulation3DGenerator {
    state: Option<StatefulState>,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blit: BlitPipeline,
}

impl FluidSimulation3DGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim3D Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_simulation_3d.wgsl").into(),
            ),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D BGL"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim3D Pipeline Layout"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("FluidSim3D Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: STATE_FORMAT,
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

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim3D Uniforms"),
            size: std::mem::size_of::<Fluid3DUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blit = BlitPipeline::new(device, target_format);

        Self {
            state: None,
            pipeline,
            bgl,
            uniform_buffer,
            sampler,
            blit,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if self.state.is_none() {
            self.state = Some(StatefulState::new(
                device, iw, ih, STATE_FORMAT, "FluidSim3D",
            ));
        }
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
        let iw = (ctx.width / 2).max(1);
        let ih = (ctx.height / 2).max(1);
        self.ensure_state(device, iw, ih);
        let state = self.state.as_mut().unwrap();

        let flow = if ctx.param_count > FLOW as u32 { ctx.params[FLOW] } else { -0.01 };
        let feather = if ctx.param_count > FEATHER as u32 { ctx.params[FEATHER] } else { 20.0 };
        let curl_angle = if ctx.param_count > CURL as u32 { ctx.params[CURL] } else { 85.0 };
        let turbulence = if ctx.param_count > TURBULENCE as u32 { ctx.params[TURBULENCE] } else { 0.001 };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let contrast = if ctx.param_count > CONTRAST as u32 { ctx.params[CONTRAST] } else { 3.5 };
        let invert = if ctx.param_count > INVERT as u32 { ctx.params[INVERT] } else { 0.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let color_mode = if ctx.param_count > COLOR as u32 { ctx.params[COLOR] } else { 0.0 };
        let color_bright = if ctx.param_count > COLOR_BRIGHT as u32 { ctx.params[COLOR_BRIGHT] } else { 2.0 };
        let container = if ctx.param_count > CONTAINER as u32 { ctx.params[CONTAINER] } else { 0.0 };
        let cam_dist = if ctx.param_count > CAM_DIST as u32 { ctx.params[CAM_DIST] } else { 3.0 };
        let cam_tilt = if ctx.param_count > CAM_TILT as u32 { ctx.params[CAM_TILT] } else { 0.3 };
        let flatten = if ctx.param_count > FLATTEN as u32 { ctx.params[FLATTEN] } else { 0.0 };

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let texel_x = 1.0 / iw as f32;
        let texel_y = 1.0 / ih as f32;

        let uniforms = Fluid3DUniforms {
            time_val: ctx.time,
            flow,
            feather,
            curl_angle,
            turbulence,
            speed,
            contrast,
            invert,
            uv_scale,
            texel_x,
            texel_y,
            color_mode,
            color_bright,
            decay: 0.97,
            cam_dist,
            cam_tilt,
            flatten,
            container,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FluidSim3D BG"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(state.read_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FluidSim3D Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: state.write_view(),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        state.swap();
        self.blit.blit(device, encoder, state.read_view(), target);

        ctx.anim_progress
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if let Some(ref mut state) = self.state {
            state.resize(device, iw, ih);
        }
    }
}
