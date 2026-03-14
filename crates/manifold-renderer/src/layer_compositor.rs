use manifold_core::BlendMode;
use crate::render_target::RenderTarget;
use crate::compositor::{Compositor, CompositorFrame};

/// Descriptor for a single clip to composite.
pub struct CompositeClipDescriptor<'a> {
    pub clip_id: &'a str,
    pub texture_view: &'a wgpu::TextureView,
    pub layer_index: i32,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale: f32,
    pub rotation: f32,
    pub invert_colors: bool,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniforms {
    blend_mode: u32,
    opacity: f32,
    translate_x: f32,
    translate_y: f32,
    scale_val: f32,
    rotation: f32,
    aspect_ratio: f32,
    invert_colors: f32,
}

/// Real compositor with ping-pong blending. Replaces ClearColorCompositor.
pub struct LayerCompositor {
    ping: RenderTarget,
    pong: RenderTarget,
    use_ping_as_source: bool,
    blend_pipeline: wgpu::RenderPipeline,
    blend_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
}

impl LayerCompositor {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        let ping = RenderTarget::new(device, width, height, format, "Compositor Ping");
        let pong = RenderTarget::new(device, width, height, format, "Compositor Pong");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Compositor Blend"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("generators/shaders/compositor_blend.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blend BGL"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Base texture (accumulator)
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
                // Blend texture (incoming clip)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blend Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Blend Pipeline"),
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
                    format,
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Blend Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Blend Uniforms"),
            size: std::mem::size_of::<BlendUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            ping,
            pong,
            use_ping_as_source: true,
            blend_pipeline,
            blend_bind_group_layout: bind_group_layout,
            sampler,
            uniform_buffer,
        }
    }

    fn source_view(&self) -> &wgpu::TextureView {
        if self.use_ping_as_source { &self.ping.view } else { &self.pong.view }
    }

    fn target_view(&self) -> &wgpu::TextureView {
        if self.use_ping_as_source { &self.pong.view } else { &self.ping.view }
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Clear the source buffer to black (start of compositing pass).
    fn clear_source(&self, encoder: &mut wgpu::CommandEncoder) {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Clear Source"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.source_view(),
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
    }

    /// Composite clips into the output buffer.
    fn composite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        clips: &[CompositeClipDescriptor],
    ) {
        // Start with clear source
        self.clear_source(encoder);

        let aspect = self.ping.width as f32 / self.ping.height as f32;

        for clip in clips {
            let uniforms = BlendUniforms {
                blend_mode: clip.blend_mode as u32,
                opacity: clip.opacity,
                translate_x: clip.translate_x,
                translate_y: clip.translate_y,
                scale_val: clip.scale,
                rotation: clip.rotation,
                aspect_ratio: aspect,
                invert_colors: if clip.invert_colors { 1.0 } else { 0.0 },
            };
            queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Blend BG"),
                layout: &self.blend_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(self.source_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(clip.texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Blend Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: self.target_view(),
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
                pass.set_pipeline(&self.blend_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            self.swap();
        }
    }
}

impl Compositor for LayerCompositor {
    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &CompositorFrame,
    ) -> &wgpu::TextureView {
        if frame.clips.is_empty() {
            // No clips — clear to black
            self.clear_source(encoder);
            return self.source_view();
        }

        self.composite(device, queue, encoder, frame.clips);
        self.source_view()
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.ping.resize(device, width, height);
        self.pong.resize(device, width, height);
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.ping.width, self.ping.height)
    }
}
