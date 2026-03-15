use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;

const MIP_LEVELS: usize = 6;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniforms {
    mode: u32,
    threshold: f32,
    intensity: f32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Per-owner mip chain for bloom.
struct BloomState {
    mips: Vec<RenderTarget>,
}

/// Bloom effect — multi-pass: prefilter → downsample chain → upsample chain → composite.
/// Stateful: per-owner 6-level mip pyramid.
pub struct BloomFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// Composite pipeline: adds bloom result to original via additive blend.
    composite_pipeline: wgpu::RenderPipeline,
    states: HashMap<i64, BloomState>,
    width: u32,
    height: u32,
}

impl BloomFX {
    pub fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        let shader_src = include_str!("shaders/bloom.wgsl");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Bloom"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Bloom BGL"),
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
            label: Some("Bloom Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Bloom Pipeline"),
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

        // Composite pipeline: same shader but with additive blending
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Bloom Composite Pipeline"),
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
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Max,
                        },
                    }),
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
            label: Some("Bloom Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bloom Uniforms"),
            size: std::mem::size_of::<BloomUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            composite_pipeline,
            states: HashMap::new(),
            width: 0,
            height: 0,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let mut mips = Vec::with_capacity(MIP_LEVELS);
            let mut w = self.width;
            let mut h = self.height;
            for i in 0..MIP_LEVELS {
                w = (w / 2).max(1);
                h = (h / 2).max(1);
                mips.push(RenderTarget::new(device, w, h, format, &format!("Bloom Mip {i}")));
            }
            self.states.insert(owner_key, BloomState { mips });
        }
    }

    fn draw_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniforms: &BloomUniforms,
        pipeline: &wgpu::RenderPipeline,
        load_op: wgpu::LoadOp<wgpu::Color>,
        label: &str,
    ) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

impl PostProcessEffect for BloomFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Bloom
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let threshold = fx.param_values.first().copied().unwrap_or(0.8);
        let intensity = fx.param_values.get(1).copied().unwrap_or(0.5);

        let state = self.states.get(&ctx.owner_key).unwrap();

        // Pass 1: Prefilter — extract bright pixels into mip[0]
        self.draw_pass(
            device, queue, encoder,
            source, &state.mips[0].view,
            &BloomUniforms {
                mode: 0,
                threshold,
                intensity,
                texel_size_x: 1.0 / ctx.width as f32,
                texel_size_y: 1.0 / ctx.height as f32,
                _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
            },
            &self.pipeline,
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            "Bloom Prefilter",
        );

        // Pass 2: Downsample chain
        for i in 1..MIP_LEVELS {
            let src_w = state.mips[i - 1].width;
            let src_h = state.mips[i - 1].height;
            self.draw_pass(
                device, queue, encoder,
                &state.mips[i - 1].view, &state.mips[i].view,
                &BloomUniforms {
                    mode: 1,
                    threshold: 0.0,
                    intensity: 0.0,
                    texel_size_x: 1.0 / src_w as f32,
                    texel_size_y: 1.0 / src_h as f32,
                    _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
                },
                &self.pipeline,
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                "Bloom Down",
            );
        }

        // Pass 3: Upsample chain (accumulate back up with additive blend)
        for i in (0..MIP_LEVELS - 1).rev() {
            let dst_w = state.mips[i].width;
            let dst_h = state.mips[i].height;
            self.draw_pass(
                device, queue, encoder,
                &state.mips[i + 1].view, &state.mips[i].view,
                &BloomUniforms {
                    mode: 2,
                    threshold: 0.0,
                    intensity,
                    texel_size_x: 1.0 / dst_w as f32,
                    texel_size_y: 1.0 / dst_h as f32,
                    _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
                },
                &self.composite_pipeline,
                wgpu::LoadOp::Load, // Preserve existing content, add bloom on top
                "Bloom Up",
            );
        }

        // Pass 4: Composite — blit source to target (passthrough), then add bloom
        // Step 1: Passthrough blit source → target (mode 3 = passthrough in shader)
        self.draw_pass(
            device, queue, encoder,
            source, target,
            &BloomUniforms {
                mode: 3, // passthrough
                threshold: 0.0,
                intensity: 0.0,
                texel_size_x: 0.0,
                texel_size_y: 0.0,
                _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
            },
            &self.pipeline,
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            "Bloom Copy Source",
        );

        // Step 2: Additively blend bloom mip[0] onto target
        self.draw_pass(
            device, queue, encoder,
            &state.mips[0].view, target,
            &BloomUniforms {
                mode: 2,
                threshold: 0.0,
                intensity,
                texel_size_x: 1.0 / ctx.width as f32,
                texel_size_y: 1.0 / ctx.height as f32,
                _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
            },
            &self.composite_pipeline,
            wgpu::LoadOp::Load, // Preserve the copied source
            "Bloom Composite",
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for state in self.states.values_mut() {
            let mut w = width;
            let mut h = height;
            for (i, mip) in state.mips.iter_mut().enumerate() {
                w = (w / 2).max(1);
                h = (h / 2).max(1);
                *mip = RenderTarget::new(device, w, h, wgpu::TextureFormat::Rgba16Float, &format!("Bloom Mip {i}"));
            }
        }
    }
}

impl StatefulEffect for BloomFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}
