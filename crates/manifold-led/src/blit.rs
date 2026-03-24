//! Minimal GPU blit pipeline for the LED edge-extend shader.
//! Self-contained — does not depend on manifold-renderer.
//! Creates a tiny render target (strip_count × leds_per_strip, Rgba8Unorm)
//! and a fullscreen triangle render pipeline with the edge-extend shader.

/// GPU resources for the LED edge-extend blit pass.
pub struct EdgeExtendBlit {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeExtendUniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    blur_radius: f32,
    _pad: f32,
}

impl EdgeExtendBlit {
    /// Create the edge-extend blit pipeline and tiny render target.
    /// `strip_count` = width, `leds_per_strip` = height of the sample texture.
    pub fn new(device: &wgpu::Device, strip_count: u32, leds_per_strip: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("LEDEdgeExtend Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/led_edge_extend.wgsl").into(),
            ),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("LEDEdgeExtend BGL"),
                entries: &[
                    // Binding 0: Uniforms
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
                    // Binding 1: Source texture (filterable — Rgba16Float from compositor
                    // is sampled via textureSample, which requires filterable)
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
                    // Binding 2: Sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("LEDEdgeExtend PipelineLayout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("LEDEdgeExtend Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
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
            label: Some("LEDEdgeExtend Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("LED_SampleRT"),
            size: wgpu::Extent3d {
                width: strip_count,
                height: leds_per_strip,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            target,
            target_view,
            width: strip_count,
            height: leds_per_strip,
        }
    }

    /// Blit the compositor output through the edge-extend shader into the tiny
    /// sample texture. The result is ready for GPU readback.
    pub fn blit(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        left_edge_width: f32,
        right_edge_width: f32,
        blur_radius: f32,
    ) {
        let uniforms = EdgeExtendUniforms {
            left_edge_width,
            right_edge_width,
            blur_radius,
            _pad: 0.0,
        };

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("LEDEdgeExtend Uniforms"),
            size: std::mem::size_of::<EdgeExtendUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("LEDEdgeExtend BindGroup"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
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
            label: Some("LEDEdgeExtend Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
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

    /// The tiny sample texture. Use for GPU readback.
    pub fn output_texture(&self) -> &wgpu::Texture {
        &self.target
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn led_edge_extend_wgsl_parses() {
        let source = include_str!("shaders/led_edge_extend.wgsl");
        naga::front::wgsl::parse_str(source).expect("WGSL parse failed");
    }
}
