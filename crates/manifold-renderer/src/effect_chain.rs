use manifold_core::EffectType;
use manifold_core::effects::{EffectGroup, EffectInstance};
use crate::effect::{EffectContext, find_chain_param};
use crate::effect_registry::EffectRegistry;
use crate::render_target::RenderTarget;
use crate::wet_dry_lerp::WetDryLerpPipeline;

/// Dispatches a chain of effects through the registry, handling group wet/dry.
///
/// Owns its own ping-pong buffers (lazy) for processing plus an internal blit
/// pipeline for copying external input views into the chain's buffers.
pub struct EffectChain {
    ping: Option<RenderTarget>,
    pong: Option<RenderTarget>,
    /// Snapshot of dry state before entering a group with wet_dry < 1.0.
    dry_snapshot: Option<RenderTarget>,
    use_ping_as_source: bool,
    /// Internal blit pipeline (Rgba16Float format) for copying input views.
    internal_blit: Option<InternalBlit>,
}

/// Lightweight blit pipeline for copying a texture view into a RenderTarget.
struct InternalBlit {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
}

const BLIT_SHADER_SRC: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
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

impl InternalBlit {
    fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("EffectChain Blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER_SRC.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("EffectChain Blit BGL"),
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
            label: Some("EffectChain Blit Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("EffectChain Blit Pipeline"),
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
            label: Some("EffectChain Blit Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self { pipeline, sampler, bind_group_layout }
    }

    /// Copy a source texture view into a target texture view.
    fn blit(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("EffectChain Blit BG"),
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
            label: Some("EffectChain Blit Pass"),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

impl Default for EffectChain {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectChain {
    pub fn new() -> Self {
        Self {
            ping: None,
            pong: None,
            dry_snapshot: None,
            use_ping_as_source: true,
            internal_blit: None,
        }
    }

    /// Ensure internal ping-pong buffers exist at the given dimensions.
    fn ensure_buffers(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let format = wgpu::TextureFormat::Rgba16Float;
        if self.ping.is_none() {
            self.ping = Some(RenderTarget::new(device, width, height, format, "EffectChain Ping"));
            self.pong = Some(RenderTarget::new(device, width, height, format, "EffectChain Pong"));
        }
        if self.internal_blit.is_none() {
            self.internal_blit = Some(InternalBlit::new(device));
        }
    }

    fn ensure_dry_snapshot(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let format = wgpu::TextureFormat::Rgba16Float;
        if self.dry_snapshot.is_none() {
            self.dry_snapshot = Some(RenderTarget::new(device, width, height, format, "EffectChain DrySnapshot"));
        }
    }

    fn source(&self) -> &RenderTarget {
        if self.use_ping_as_source {
            self.ping.as_ref().unwrap()
        } else {
            self.pong.as_ref().unwrap()
        }
    }

    fn target(&self) -> &RenderTarget {
        if self.use_ping_as_source {
            self.pong.as_ref().unwrap()
        } else {
            self.ping.as_ref().unwrap()
        }
    }

    fn source_view(&self) -> &wgpu::TextureView {
        &self.source().view
    }

    fn target_view(&self) -> &wgpu::TextureView {
        &self.target().view
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Apply a chain of effects. Returns the texture view with the final result.
    ///
    /// If the chain is empty or has no enabled effects, returns `None` (caller
    /// should use the original input).
    ///
    /// `mid_chain_tap`: Optional callback index for external output (LED walls).
    /// Unity ref: CompositorStack.cs lines 864-865, 918-920
    #[allow(clippy::too_many_arguments)]
    pub fn apply_chain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        registry: &mut EffectRegistry,
        input_view: &wgpu::TextureView,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
        wet_dry_lerp: Option<&WetDryLerpPipeline>,
    ) -> Option<&wgpu::TextureView> {
        // Filter to enabled effects with registered processors
        let enabled: Vec<usize> = effects
            .iter()
            .enumerate()
            .filter(|(_, fx)| {
                if !fx.enabled { return false; }
                if registry.get_mut(fx.effect_type).is_none() {
                    log::debug!("Effect {:?} has no GPU processor — skipped", fx.effect_type);
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        if enabled.is_empty() {
            return None;
        }

        self.ensure_buffers(device, ctx.width, ctx.height);
        self.use_ping_as_source = true;

        // Precompute cross-chain params for effects that need them.
        // Unity ref: EffectContext.FindChainParam() — VoronoiPrism reads EdgeStretch width.
        let chain_ctx = EffectContext {
            edge_stretch_width: find_chain_param(
                effects, EffectType::EdgeStretch, 1, 0.5625,
            ),
            ..*ctx
        };

        // Copy input into our source buffer via blit (handles any input format)
        self.internal_blit.as_ref().unwrap().blit(
            device, encoder, input_view, self.source_view(),
        );

        let mut current_group_id: Option<&str> = None;

        for &idx in &enabled {
            let fx = &effects[idx];

            // Track group transitions for wet/dry
            let fx_group_id = fx.group_id.as_deref();
            if fx_group_id != current_group_id {
                // Leaving a group — apply wet/dry lerp if needed
                if let Some(prev_gid) = current_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                        self.apply_wet_dry_lerp(
                            device, queue, encoder, group.wet_dry, wet_dry_lerp,
                        );
                    }

                // Entering a new group — snapshot dry state if wet_dry < 1.0
                if let Some(gid) = fx_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == gid) {
                        if !group.enabled {
                            current_group_id = Some(gid);
                            continue;
                        }
                        if group.wet_dry < 1.0 {
                            self.ensure_dry_snapshot(device, ctx.width, ctx.height);
                            // GPU copy source → dry_snapshot
                            copy_rt_to_rt(
                                encoder,
                                self.source(),
                                self.dry_snapshot.as_ref().unwrap(),
                            );
                        }
                    }

                current_group_id = fx_group_id;
            }

            // Check if group is disabled — skip effect
            if let Some(gid) = fx_group_id
                && let Some(group) = groups.iter().find(|g| g.id == gid)
                    && !group.enabled {
                        continue;
                    }

            // Apply the effect (skip if ShouldSkip — no GPU work, no swap)
            // Unity ref: CompositorStack checks ShouldSkip before Apply + buffer swap.
            if let Some(processor) = registry.get_mut(fx.effect_type)
                && !processor.should_skip(fx) {
                    processor.apply(
                        device, queue, encoder,
                        self.source_view(),
                        self.target_view(),
                        fx, &chain_ctx,
                    );
                    self.swap();
                }
        }

        // Final group exit — apply wet/dry if needed
        if let Some(prev_gid) = current_group_id
            && let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                self.apply_wet_dry_lerp(
                    device, queue, encoder, group.wet_dry, wet_dry_lerp,
                );
            }

        Some(self.source_view())
    }

    /// Apply wet/dry lerp if wet_dry < 1.0 and dry snapshot exists.
    fn apply_wet_dry_lerp(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        wet_dry: f32,
        lerp_pipeline: Option<&WetDryLerpPipeline>,
    ) {
        if wet_dry >= 1.0 {
            return;
        }
        let dry_snap = match &self.dry_snapshot {
            Some(snap) => snap,
            None => return,
        };
        let lerp = match lerp_pipeline {
            Some(l) => l,
            None => return,
        };

        // Lerp: dry_snapshot (dry) + source (wet) → target
        lerp.apply(
            device, queue, encoder,
            &dry_snap.view,
            self.source_view(),
            self.target_view(),
            wet_dry,
        );
        self.swap();
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if let Some(ping) = &mut self.ping {
            ping.resize(device, width, height);
        }
        if let Some(pong) = &mut self.pong {
            pong.resize(device, width, height);
        }
        if let Some(snap) = &mut self.dry_snapshot {
            snap.resize(device, width, height);
        }
    }
}

/// GPU-side texture copy between two RenderTargets using wgpu's copy command.
fn copy_rt_to_rt(
    encoder: &mut wgpu::CommandEncoder,
    source: &RenderTarget,
    target: &RenderTarget,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: source.width.min(target.width),
            height: source.height.min(target.height),
            depth_or_array_layers: 1,
        },
    );
}
