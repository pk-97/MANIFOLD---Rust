/// ACES tonemapping pipeline — mechanical translation of Unity's
/// CompositorStack.ApplyTonemap() + ACESTonemap.shader.
///
/// Owned by the compositor. Applied as the final step after master effects,
/// before the blit to the display surface.

use crate::render_target::RenderTarget;

/// Per-frame tonemap settings. Matches Unity CompositorStack properties:
/// TonemapExposure, HDROutputEnabled, PaperWhiteNits, MaxDisplayNits.
#[derive(Debug, Clone, Copy)]
pub struct TonemapSettings {
    /// Exposure multiplier for ACES tonemapping. 1.0 = neutral.
    /// Matches Unity CompositorStack.TonemapExposure.
    pub exposure: f32,
    /// HDR output mode. false = SDR (sRGB tonemap), true = HDR display-linear (EDR).
    /// Matches Unity CompositorStack.HDROutputEnabled.
    pub hdr_output_enabled: bool,
    /// Paper white in nits (scene 1.0 maps to this). Typical: 200 nits.
    /// Matches Unity CompositorStack.PaperWhiteNits.
    pub paper_white_nits: f32,
    /// Display maximum luminance in nits. HDR TVs: 1000, LED walls: 5000+.
    /// Matches Unity CompositorStack.MaxDisplayNits.
    pub max_display_nits: f32,
}

impl Default for TonemapSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            hdr_output_enabled: false,
            paper_white_nits: 200.0,
            max_display_nits: 1000.0,
        }
    }
}

/// Uniform buffer layout for the tonemap shader.
/// 16 bytes, naturally aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TonemapUniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32, // 0 = SDR, 1 = PQ, 2 = EDR
}

/// GPU pipeline for ACES tonemapping.
/// Follows the exact pattern of WetDryLerpPipeline.
pub struct TonemapPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// Tonemap output buffer. Matches Unity's tonemappedOutput RenderTexture.
    /// Separate from the compositor's main buffer so PreTonemapOutput survives.
    pub output: RenderTarget,
}

impl TonemapPipeline {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ACES Tonemap Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("effects/shaders/aces_tonemap.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Tonemap BGL"),
            entries: &[
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
            label: Some("Tonemap Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Tonemap Pipeline"),
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
            label: Some("Tonemap Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Tonemap Uniforms"),
            size: std::mem::size_of::<TonemapUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output = RenderTarget::new(device, width, height, format, "TonemappedOutput");

        Self { pipeline, bind_group_layout, sampler, uniform_buffer, output }
    }

    /// Apply ACES tonemapping to the HDR source buffer.
    /// Matches Unity CompositorStack.ApplyTonemap().
    ///
    /// Realtime display uses SDR (mode 0) or EDR (mode 2) depending on
    /// hdr_output_enabled. PQ (mode 1) is reserved for export pipeline.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        hdr_source: &wgpu::TextureView,
        settings: &TonemapSettings,
    ) {
        // Realtime HDR preview uses display-linear EDR pass (2).
        // Pass 1 remains reserved for explicit PQ workflows (e.g. export).
        let mode = if settings.hdr_output_enabled { 2u32 } else { 0u32 };

        let uniforms = TonemapUniforms {
            exposure: settings.exposure,
            paper_white: settings.paper_white_nits,
            max_nits: settings.max_display_nits,
            mode,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Tonemap BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(hdr_source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Tonemap Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.output.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

    /// Resize the tonemap output buffer. Matches Unity's lazy reallocation in
    /// ApplyTonemap() when hdrSource dimensions change.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.output.resize(device, width, height);
    }
}
