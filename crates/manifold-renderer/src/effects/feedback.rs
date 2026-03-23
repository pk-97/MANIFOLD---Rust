use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use super::simple_blit_helper::SimpleBlitHelper;

const PASSTHROUGH_SHADER: &str = r#"
struct Uniforms { _pad: vec4<f32>, }
@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, }
@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source_tex, tex_sampler, in.uv);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FeedbackUniforms {
    feedback_amount: f32,
    _pad: [f32; 3],
}

/// Per-owner state: the previous frame's feedback buffer.
struct FeedbackState {
    buffer: RenderTarget,
}

/// Feedback effect — lerps current frame with previous frame's state buffer.
/// Stateful: maintains one feedback buffer per owner (clip/layer/master).
pub struct FeedbackFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// Passthrough blit for copying result into feedback state buffer.
    copy_blit: SimpleBlitHelper,
    states: AHashMap<i64, FeedbackState>,
    width: u32,
    height: u32,
}

/// Clear a RenderTarget to transparent black via a render pass.
/// Unity ref: RenderTextureUtil.Clear() — zeros texture contents.
fn clear_render_target(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Clear RT"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
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

impl FeedbackFX {
    pub fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Feedback"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/feedback.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Feedback BGL"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Feedback Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Feedback Pipeline"),
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
            label: Some("Feedback Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Feedback Uniforms"),
            size: std::mem::size_of::<FeedbackUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let copy_blit = SimpleBlitHelper::new(
            device,
            PASSTHROUGH_SHADER,
            "Feedback Copy",
            16, // vec4 pad
        );

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            copy_blit,
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    /// Create state buffer and clear to black.
    /// Unity ref: GetOrCreateState + RenderTextureUtil.Clear()
    fn ensure_state(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let buffer = RenderTarget::new(device, self.width, self.height, format, "Feedback State");
            // Clear to black so first-frame shader reads black prev buffer,
            // producing mix(current, black, amount) — matching Unity behavior.
            clear_render_target(encoder, &buffer.view);
            self.states.insert(owner_key, FeedbackState { buffer });
        }
    }
}

impl PostProcessEffect for FeedbackFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::FEEDBACK
    }

    // ShouldSkip: default (param[0] <= 0) — matches Unity SimpleBlitEffect.ShouldSkip.

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, encoder, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();

        // FeedbackFX.cs:34 — Mathf.Min(fx.GetParam(0), 0.98f)
        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.0).min(0.98);
        let uniforms = FeedbackUniforms {
            feedback_amount,
            _pad: [0.0; 3],
        };

        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Feedback BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&state.buffer.view),
                },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.render_timestamps("Feedback Pass", ctx.width, ctx.height));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Feedback Pass"),
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
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // PostBlit: copy blended result into feedback state buffer for next frame.
        // Unity ref: Graphics.CopyTexture(result, stateBuffer)
        let state = self.states.get(&ctx.owner_key).unwrap();
        self.copy_blit.draw(
            device, queue, encoder,
            target, &state.buffer.view,
            &[0u8; 16],
            "Feedback State Copy",
            ctx.width, ctx.height,
            profiler,
        );
    }

    // FeedbackFX.cs lines 42-46 — ClearState: zeros ALL state buffer contents.
    // Without an encoder, we remove entries so they get re-created (and cleared to
    // black) on the next ensure_state call.
    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.states.clear();
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for FeedbackFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        // Unity: RenderTextureUtil.Clear(rt) — zeros contents.
        // Remove entry so it re-creates cleared on next ensure_state.
        self.states.remove(&owner_key);
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
