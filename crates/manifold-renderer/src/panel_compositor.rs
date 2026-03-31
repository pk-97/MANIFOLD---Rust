use manifold_ui::node::Rect;

/// Composites cached panel textures onto the surface.
///
/// Each panel is drawn as a viewport-restricted fullscreen triangle blit,
/// reusing the same shader pattern as BlitPipeline. ~7 draw calls per frame.
pub struct PanelCompositor {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

const BLIT_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
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

impl PanelCompositor {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Panel Compositor Shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Panel Compositor BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
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
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Panel Compositor Layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Panel Compositor"),
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
                        format,
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
            label: Some("Panel Compositor Sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }

    /// Bind group layout for creating panel cache bind groups.
    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    /// Sampler for creating panel cache bind groups.
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// Composite cached panel textures into an existing render pass.
    /// Each panel is drawn by setting the viewport to its screen rect
    /// and drawing a fullscreen triangle.
    pub fn draw_panels<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        panels: &[(Rect, &'a wgpu::BindGroup)],
        scale_factor: f32,
    ) {
        pass.set_pipeline(&self.pipeline);
        for (rect, bind_group) in panels {
            let px = rect.x * scale_factor;
            let py = rect.y * scale_factor;
            let pw = rect.width * scale_factor;
            let ph = rect.height * scale_factor;
            if pw <= 0.0 || ph <= 0.0 {
                continue;
            }
            pass.set_viewport(px, py, pw, ph, 0.0, 1.0);
            pass.set_bind_group(0, *bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}
