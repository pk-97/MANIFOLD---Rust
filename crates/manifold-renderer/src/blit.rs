/// Fullscreen triangle blit pipeline: renders a texture onto a surface.
pub struct BlitPipeline {
    #[allow(dead_code)] // render fallback for non-hal builds
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    #[allow(dead_code)]
    bind_group_layout: wgpu::BindGroupLayout,
    // Compute blit pipeline (macOS + hal-encoding)
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_bgl: wgpu::BindGroupLayout,
}

const BLIT_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle from vertex index (no vertex buffer needed)
    var out: VertexOutput;
    let x = f32(i32(vertex_index) / 2) * 4.0 - 1.0;
    let y = f32(i32(vertex_index) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;

impl BlitPipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blit Shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blit Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blit Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Blit Pipeline"),
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
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
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
            label: Some("Blit Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── Compute blit pipeline (macOS + hal-encoding) ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (compute_pipeline, compute_bgl) = {
            let compute_shader =
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("Blit Compute Shader"),
                    source: wgpu::ShaderSource::Wgsl(
                        include_str!("generators/shaders/blit_compute.wgsl")
                            .into(),
                    ),
                });

            let cbgl = device.create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some("Blit Compute BGL"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type:
                                    wgpu::TextureSampleType::Float {
                                        filterable: true,
                                    },
                                view_dimension:
                                    wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Sampler(
                                wgpu::SamplerBindingType::Filtering,
                            ),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::StorageTexture {
                                access:
                                    wgpu::StorageTextureAccess::WriteOnly,
                                format: wgpu::TextureFormat::Rgba16Float,
                                view_dimension:
                                    wgpu::TextureViewDimension::D2,
                            },
                            count: None,
                        },
                    ],
                },
            );

            let clayout = device.create_pipeline_layout(
                &wgpu::PipelineLayoutDescriptor {
                    label: Some("Blit Compute Layout"),
                    bind_group_layouts: &[&cbgl],
                    immediate_size: 0,
                },
            );

            let cpipeline = device.create_compute_pipeline(
                &wgpu::ComputePipelineDescriptor {
                    label: Some("Blit Compute Pipeline"),
                    layout: Some(&clayout),
                    module: &compute_shader,
                    entry_point: Some("cs_main"),
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                },
            );

            (cpipeline, cbgl)
        };

        Self {
            pipeline,
            sampler,
            bind_group_layout,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_bgl,
        }
    }

    /// Blit a source texture view onto a target texture view.
    /// `target_width`/`target_height` are needed for compute dispatch sizing.
    pub fn blit(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
    ) {
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            let bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Blit Compute Bind Group"),
                    layout: &self.compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(
                                source,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(
                                target,
                            ),
                        },
                    ],
                });

            let mut pass =
                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Blit Compute Pass"),
                    timestamp_writes: None,
                });
            pass.set_pipeline(&self.compute_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                target_width.div_ceil(16),
                target_height.div_ceil(16),
                1,
            );
        }

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            let _ = (target_width, target_height);
            let bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Blit Bind Group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(
                                source,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                    ],
                });

            let mut pass =
                encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Blit Pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(
                                    wgpu::Color::BLACK,
                                ),
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Blit source into a specific rect within the target (in physical pixels).
    /// The source texture fills the rect, maintaining aspect ratio via viewport.
    pub fn blit_to_rect(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Blit Rect Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Blit Rect Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // Don't clear — preserve existing content
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_viewport(x, y, width, height, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Blit source into target rect, preserving source aspect ratio (letterbox/pillarbox).
    /// Equivalent to Unity's AspectRatioFitter with FitInParent mode.
    /// `source_aspect` = source_width / source_height.
    pub fn blit_to_rect_fit(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        rect_x: f32,
        rect_y: f32,
        rect_w: f32,
        rect_h: f32,
        source_aspect: f32,
    ) {
        if rect_w <= 0.0 || rect_h <= 0.0 || source_aspect <= 0.0 {
            return;
        }
        let rect_aspect = rect_w / rect_h;
        let (fit_w, fit_h) = if source_aspect > rect_aspect {
            // Source wider than rect — fit to width, letterbox top/bottom
            (rect_w, rect_w / source_aspect)
        } else {
            // Source taller than rect — fit to height, pillarbox left/right
            (rect_h * source_aspect, rect_h)
        };
        let fit_x = rect_x + (rect_w - fit_w) * 0.5;
        let fit_y = rect_y + (rect_h - fit_h) * 0.5;
        self.blit_to_rect(device, encoder, source, target, fit_x, fit_y, fit_w, fit_h);
    }
}
