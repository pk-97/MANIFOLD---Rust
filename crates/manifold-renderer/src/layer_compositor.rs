use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use manifold_core::BlendMode;
use manifold_core::effects::{EffectGroup, EffectInstance};
use crate::effect::EffectContext;
use crate::effect_chain::EffectChain;
use crate::effect_registry::EffectRegistry;
use crate::render_target::RenderTarget;
use crate::tonemap::TonemapPipeline;
use crate::wet_dry_lerp::WetDryLerpPipeline;
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
    pub effects: &'a [EffectInstance],
    pub effect_groups: &'a [EffectGroup],
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

/// Shared GPU resources for blend operations. Extracted from the compositor
/// so they can be borrowed independently from ping-pong buffers.
struct BlendResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
}

impl BlendResources {
    fn new(device: &wgpu::Device) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Compositor Blend"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("generators/shaders/compositor_blend.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blend BGL"),
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
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

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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

        Self { pipeline, bind_group_layout, sampler, uniform_buffer }
    }

    /// Execute a blend pass: reads source + blend textures, writes to target.
    fn blend_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
        blend_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniforms: &BlendUniforms,
    ) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Blend BG"),
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
                    resource: wgpu::BindingResource::TextureView(blend_view),
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
                    view: target_view,
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
}

/// Standalone ping-pong buffer pair. Can be borrowed independently
/// from other compositor state (avoids borrow conflicts).
struct PingPong {
    ping: RenderTarget,
    pong: RenderTarget,
    use_ping_as_source: bool,
}

impl PingPong {
    fn new(device: &wgpu::Device, width: u32, height: u32, label_prefix: &str) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        Self {
            ping: RenderTarget::new(device, width, height, format, &format!("{label_prefix} Ping")),
            pong: RenderTarget::new(device, width, height, format, &format!("{label_prefix} Pong")),
            use_ping_as_source: true,
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

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.ping.resize(device, width, height);
        self.pong.resize(device, width, height);
    }

    fn width(&self) -> u32 { self.ping.width }
    fn height(&self) -> u32 { self.ping.height }

    /// Clear source buffer. `opaque` = true clears to opaque black (a=1),
    /// false clears to transparent black (a=0).
    fn clear_source(&self, encoder: &mut wgpu::CommandEncoder, opaque: bool) {
        let color = if opaque {
            wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }
        } else {
            wgpu::Color::TRANSPARENT
        };
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.source_view(),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
}

/// Deterministic hash of a clip ID string to produce an owner_key for effect context.
fn clip_id_owner_key(clip_id: &str) -> i64 {
    let mut hasher = DefaultHasher::new();
    clip_id.hash(&mut hasher);
    hasher.finish() as i64
}

/// Check if an effect slice has any enabled effects with non-zero amount.
/// Unity ref: CompositorStack.cs lines 965-974 — checks enabled && GetParam(0) > 0.
fn has_enabled_effects(effects: &[EffectInstance]) -> bool {
    for fx in effects {
        if fx.enabled && fx.param_values.first().copied().unwrap_or(0.0) > 0.0 {
            return true;
        }
    }
    false
}

/// Layer-aware compositor with per-layer ping-pong blending.
///
/// Compositing flow:
/// 1. Clear main buffer to opaque black
/// 2. Group clips by layer_index (sorted descending by engine)
/// 3. For each layer:
///    - Single clip: blit directly into main with layer blend mode
///    - Multi-clip: composite clips into layer buffer (Normal blend), then
///      blit layer result into main with layer blend mode
/// 4. Apply master effects to final buffer
/// 5. Return final accumulated main buffer
pub struct LayerCompositor {
    /// Main accumulation ping-pong (opaque black init).
    main: PingPong,
    /// Shared per-layer scratch buffers (lazy, transparent black init).
    layer_buf: Option<PingPong>,
    /// GPU resources for blend operations (pipeline, sampler, uniforms).
    blend: BlendResources,
    /// Effect chain processor (owns its own ping-pong for effect processing).
    effect_chain: EffectChain,
    /// Registry of all effect processors.
    effect_registry: EffectRegistry,
    /// Wet/dry lerp pipeline for effect group blending.
    wet_dry_lerp: WetDryLerpPipeline,
    /// ACES tonemapping pipeline. Matches Unity's CompositorStack.tonemapMaterial +
    /// tonemappedOutput. Applied as the final step after master effects.
    tonemap: TonemapPipeline,
}

impl LayerCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) -> Self {
        Self {
            main: PingPong::new(device, width, height, "Compositor"),
            layer_buf: None,
            blend: BlendResources::new(device),
            effect_chain: EffectChain::new(),
            effect_registry: EffectRegistry::new(device, queue),
            wet_dry_lerp: WetDryLerpPipeline::new(device),
            tonemap: TonemapPipeline::new(device, width, height),
        }
    }

    /// Ensure lazy layer scratch buffers exist.
    fn ensure_layer_buffers(&mut self, device: &wgpu::Device) {
        if self.layer_buf.is_none() {
            let w = self.main.width();
            let h = self.main.height();
            self.layer_buf = Some(PingPong::new(device, w, h, "Layer Scratch"));
        }
    }

    /// Apply effect chain to the given input view, returning the processed view
    /// if any effects were applied, or None if the input should be used as-is.
    fn apply_effects<'a>(
        effect_chain: &'a mut EffectChain,
        registry: &mut EffectRegistry,
        wet_dry_lerp: &WetDryLerpPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input_view: &'a wgpu::TextureView,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> Option<&'a wgpu::TextureView> {
        effect_chain.apply_chain(
            device, queue, encoder,
            registry,
            input_view,
            effects,
            groups,
            ctx,
            Some(wet_dry_lerp),
        )
    }

    /// Composite all clips into main buffer, grouping by layer.
    fn composite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &CompositorFrame,
    ) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();
        let aspect = width as f32 / height as f32;

        // Clear main to opaque black
        self.main.clear_source(encoder, true);

        // Check for any solo layer
        let any_solo = frame.layers.iter().any(|l| l.is_solo);

        // Group clips by layer_index. Clips are sorted by layer_index descending
        // (higher index = bottom of timeline = rendered first as base).
        let mut i = 0;
        while i < clips.len() {
            let layer_idx = clips[i].layer_index;

            // Find layer descriptor
            let layer_desc = frame.layers.iter().find(|l| l.layer_index == layer_idx);

            // Check mute/solo
            if let Some(ld) = layer_desc {
                if ld.is_muted || (any_solo && !ld.is_solo) {
                    while i < clips.len() && clips[i].layer_index == layer_idx {
                        i += 1;
                    }
                    continue;
                }
            }

            // Count clips in this layer group
            let group_start = i;
            while i < clips.len() && clips[i].layer_index == layer_idx {
                i += 1;
            }
            let group = &clips[group_start..i];

            // Get layer blend mode and opacity
            let layer_blend = layer_desc.map_or(BlendMode::Normal, |l| l.blend_mode);
            let layer_opacity = layer_desc.map_or(1.0, |l| l.opacity);

            // Check if this layer has layer-level effects (Unity: CompositorStack.cs lines 414-449)
            let has_layer_effects = layer_desc
                .map_or(false, |ld| has_enabled_effects(ld.effects));

            if group.len() == 1 && !has_layer_effects {
                // Single clip with NO layer effects: blit directly into main with layer blend mode
                let clip = &group[0];

                // Apply clip-level effects if present
                let blend_input = if has_enabled_effects(clip.effects) {
                    let ctx = EffectContext {
                        time: frame.time,
                        beat: frame.beat,
                        dt: frame.dt,
                        width,
                        height,
                        owner_key: clip_id_owner_key(clip.clip_id),
                        is_clip_level: true, edge_stretch_width: 0.5625,
                        frame_count: frame.frame_count as i64,
                    };
                    Self::apply_effects(
                        &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                        device, queue, encoder,
                        clip.texture_view, clip.effects, clip.effect_groups, &ctx,
                    )
                } else {
                    None
                };
                let effective_blend_view = blend_input.unwrap_or(clip.texture_view);

                let uniforms = BlendUniforms {
                    blend_mode: layer_blend as u32,
                    opacity: layer_opacity * clip.opacity,
                    translate_x: clip.translate_x,
                    translate_y: clip.translate_y,
                    scale_val: clip.scale,
                    rotation: clip.rotation,
                    aspect_ratio: aspect,
                    invert_colors: if clip.invert_colors { 1.0 } else { 0.0 },
                };

                self.blend.blend_pass(
                    device, queue, encoder,
                    self.main.source_view(),
                    effective_blend_view,
                    self.main.target_view(),
                    &uniforms,
                );
                self.main.swap();
            } else {
                // Multi-clip: composite into layer buffer, then into main
                self.ensure_layer_buffers(device);
                let layer_buf = self.layer_buf.as_mut().unwrap();

                // Clear layer buffer to transparent
                layer_buf.clear_source(encoder, false);

                // Composite each clip into layer buffer with Normal blend
                for clip in group {
                    // Apply clip-level effects if present
                    let blend_input = if has_enabled_effects(clip.effects) {
                        let ctx = EffectContext {
                            time: frame.time,
                            beat: frame.beat,
                            dt: frame.dt,
                            width,
                            height,
                            owner_key: clip_id_owner_key(clip.clip_id),
                            is_clip_level: true, edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        Self::apply_effects(
                            &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                            device, queue, encoder,
                            clip.texture_view, clip.effects, clip.effect_groups, &ctx,
                        )
                    } else {
                        None
                    };
                    let effective_blend_view = blend_input.unwrap_or(clip.texture_view);

                    let uniforms = BlendUniforms {
                        blend_mode: BlendMode::Normal as u32,
                        opacity: clip.opacity,
                        translate_x: clip.translate_x,
                        translate_y: clip.translate_y,
                        scale_val: clip.scale,
                        rotation: clip.rotation,
                        aspect_ratio: aspect,
                        invert_colors: if clip.invert_colors { 1.0 } else { 0.0 },
                    };

                    self.blend.blend_pass(
                        device, queue, encoder,
                        layer_buf.source_view(),
                        effective_blend_view,
                        layer_buf.target_view(),
                        &uniforms,
                    );
                    layer_buf.swap();
                }

                // Apply layer-level effects to composited layer buffer
                let layer_source = if let Some(ld) = layer_desc {
                    if has_enabled_effects(ld.effects) {
                        let ctx = EffectContext {
                            time: frame.time,
                            beat: frame.beat,
                            dt: frame.dt,
                            width,
                            height,
                            owner_key: (layer_idx as i64) + 1,
                            is_clip_level: false, edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        let layer_buf = self.layer_buf.as_ref().unwrap();
                        Self::apply_effects(
                            &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                            device, queue, encoder,
                            layer_buf.source_view(), ld.effects, ld.effect_groups, &ctx,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                let layer_buf = self.layer_buf.as_ref().unwrap();
                let effective_layer_view = layer_source.unwrap_or(layer_buf.source_view());

                // Blit layer result into main with layer blend mode (no transforms)
                let uniforms = BlendUniforms {
                    blend_mode: layer_blend as u32,
                    opacity: layer_opacity,
                    translate_x: 0.0,
                    translate_y: 0.0,
                    scale_val: 1.0,
                    rotation: 0.0,
                    aspect_ratio: aspect,
                    invert_colors: 0.0,
                };

                self.blend.blend_pass(
                    device, queue, encoder,
                    self.main.source_view(),
                    effective_layer_view,
                    self.main.target_view(),
                    &uniforms,
                );
                self.main.swap();
            }
        }

        // Apply master effects to final composited buffer
        if has_enabled_effects(frame.master_effects) {
            let ctx = EffectContext {
                time: frame.time,
                beat: frame.beat,
                dt: frame.dt,
                width,
                height,
                owner_key: 0, // master
                is_clip_level: false, edge_stretch_width: 0.5625,
                frame_count: frame.frame_count as i64,
            };
            if let Some(processed) = Self::apply_effects(
                &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                device, queue, encoder,
                self.main.source_view(), frame.master_effects, frame.master_effect_groups, &ctx,
            ) {
                // Blit effect chain result back into main via Opaque blend (full replace).
                // source_view (t_base) and target_view are always different textures (ping/pong).
                // processed points to effect_chain's internal buffer (third texture). No hazard.
                let uniforms = BlendUniforms {
                    blend_mode: BlendMode::Opaque as u32,
                    opacity: 1.0,
                    translate_x: 0.0,
                    translate_y: 0.0,
                    scale_val: 1.0,
                    rotation: 0.0,
                    aspect_ratio: aspect,
                    invert_colors: 0.0,
                };
                self.blend.blend_pass(
                    device, queue, encoder,
                    self.main.source_view(),
                    processed,
                    self.main.target_view(),
                    &uniforms,
                );
                self.main.swap();
            }
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
            self.main.clear_source(encoder, true);
        } else {
            self.composite(device, queue, encoder, frame);
        }

        // PreTonemapOutput = main.source_view() (preserved — tonemap writes to its own RT)
        // ApplyTonemap(finalBuffer) → tonemap.output
        self.tonemap.apply(
            device, queue, encoder,
            self.main.source_view(),
            &frame.tonemap,
        );
        &self.tonemap.output.view
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.main.resize(device, width, height);
        if let Some(lb) = &mut self.layer_buf {
            lb.resize(device, width, height);
        }
        self.effect_chain.resize(device, width, height);
        self.effect_registry.resize_all(device, width, height);
        self.tonemap.resize(device, width, height);
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.main.width(), self.main.height())
    }

    fn pre_tonemap_output(&self) -> &wgpu::TextureView {
        self.main.source_view()
    }

    fn output_texture(&self) -> &wgpu::Texture {
        &self.tonemap.output.texture
    }
}
