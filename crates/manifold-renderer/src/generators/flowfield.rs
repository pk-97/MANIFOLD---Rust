use manifold_core::GeneratorType;
use crate::blit::BlitPipeline;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::stateful_base::StatefulState;

// Parameter indices matching types.rs param_defs
const NOISE: usize = 0;
const CURL: usize = 1;
const DECAY: usize = 2;
const SPEED: usize = 3;
const SCALE: usize = 4;
const SNAP: usize = 5;

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// Snap presets: cycled by trigger_count
const PRESET_NOISE: [f32; 6] = [2.0, 4.0, 7.0, 1.5, 8.0, 10.0];
const PRESET_CURL: [f32; 6] = [0.8, 0.4, 1.6, 0.3, 1.0, 1.8];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FlowfieldUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    noise_scale: f32,
    curl_intensity: f32,
    decay: f32,
    texel_x: f32,
    texel_y: f32,
    _pad: [f32; 2],
}

pub struct FlowfieldGenerator {
    state: Option<StatefulState>,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blit: BlitPipeline,
}

impl FlowfieldGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Flowfield Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Flowfield Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/flowfield.wgsl").into(),
            ),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flowfield BGL"),
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
            label: Some("Flowfield Pipeline Layout"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        // Simulation renders into STATE_FORMAT (Rgba16Float)
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Flowfield Pipeline"),
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
            label: Some("Flowfield Uniforms"),
            size: std::mem::size_of::<FlowfieldUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Blit pipeline to upscale half-res state to full-res output
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
                device, iw, ih, STATE_FORMAT, "Flowfield",
            ));
        }
    }
}

impl Generator for FlowfieldGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Flowfield
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

        let mut noise_scale = if ctx.param_count > NOISE as u32 { ctx.params[NOISE] } else { 1.5 };
        let mut curl_intensity = if ctx.param_count > CURL as u32 { ctx.params[CURL] } else { 0.3 };
        let decay = if ctx.param_count > DECAY as u32 { ctx.params[DECAY] } else { 0.97 };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let snap = if ctx.param_count > SNAP as u32 { ctx.params[SNAP] } else { 1.0 };

        // Snap presets override noise and curl based on trigger_count
        if snap > 0.5 {
            let idx = (ctx.trigger_count as usize) % PRESET_NOISE.len();
            noise_scale = PRESET_NOISE[idx];
            curl_intensity = PRESET_CURL[idx];
        }

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let texel_x = 1.0 / iw as f32;
        let texel_y = 1.0 / ih as f32;

        let uniforms = FlowfieldUniforms {
            time: ctx.time,
            beat: ctx.beat,
            aspect_ratio: ctx.aspect,
            anim_speed: speed,
            uv_scale,
            noise_scale,
            curl_intensity,
            decay,
            texel_x,
            texel_y,
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // Single simulation step (combined sim+display in shader)
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Flowfield BG"),
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
                label: Some("Flowfield Sim Pass"),
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

        // Blit half-res state to full-res output (with bilinear upscale)
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
