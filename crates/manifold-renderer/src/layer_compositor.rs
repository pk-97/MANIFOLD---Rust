use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use manifold_core::{BlendMode, EffectTypeId};
use manifold_core::effects::{EffectGroup, EffectInstance};
use crate::effect::EffectContext;
use crate::effect_chain::EffectChain;
use crate::effect_registry::EffectRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::tonemap::TonemapPipeline;
use crate::uniform_arena::UniformArena;
use crate::wet_dry_lerp::WetDryLerpPipeline;
use crate::compositor::{Compositor, CompositorFrame};

/// Descriptor for a single clip to composite.
pub struct CompositeClipDescriptor<'a> {
    pub clip_id: &'a str,
    pub texture_view: &'a wgpu::TextureView,
    pub texture: &'a wgpu::Texture,
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

const _: () = assert!(std::mem::size_of::<BlendUniforms>() == 32);

/// Shared GPU resources for blend operations. Extracted from the compositor
/// so they can be borrowed independently from ping-pong buffers.
///
/// Uses a ring buffer with dynamic uniform offsets so each blend pass within
/// a frame reads its own uniforms. Without this, `queue.write_buffer` batches
/// all writes before GPU execution, causing every pass to read the LAST
/// written value — breaking per-layer blend modes.
///
/// The buffer grows dynamically when a frame needs more slots than currently
/// allocated, so there is no hard limit on project complexity.
/// Compute-based blend resources. Uses compute dispatches instead of render
/// passes to bypass Metal TBDR tile overhead (~290us per pass at 4K).
struct BlendResources {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Compositor width/height — needed for dispatch_workgroups.
    width: u32,
    height: u32,
}

impl BlendResources {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Compositor Blend Compute"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("generators/shaders/compositor_blend_compute.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blend Compute BGL"),
            entries: &[
                // binding 0: uniforms (dynamic offset)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<BlendUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                // binding 1: base texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 2: blend texture
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 3: sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 4: output storage texture (write-only)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blend Compute Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Blend Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Blend Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline, bind_group_layout, sampler,
            width, height,
        }
    }

    /// Execute a compute blend: reads source + blend textures, writes to target storage texture.
    /// Pushes uniforms into the shared arena (flushed once per frame) instead of
    /// calling queue.write_buffer per pass — reduces wgpu staging overhead.
    fn blend_pass(
        &self,
        gpu: &mut GpuEncoder,
        arena: &mut UniformArena,
        source_view: &wgpu::TextureView,
        blend_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniforms: &BlendUniforms,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let offset = arena.push(uniforms);

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Blend Compute BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: arena.buffer(),
                        offset: 0,
                        size: wgpu::BufferSize::new(
                            std::mem::size_of::<BlendUniforms>() as u64,
                        ),
                    }),
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
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(target_view),
                },
            ],
        });

        let ts = profiler.and_then(|p| p.compute_timestamps("Blend Pass", self.width, self.height));
        let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Blend Pass"),
            timestamp_writes: ts,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[offset as u32]);
        pass.dispatch_workgroups(
            self.width.div_ceil(16),
            self.height.div_ceil(16),
            1,
        );
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
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

    fn source_texture(&self) -> &wgpu::Texture {
        if self.use_ping_as_source { &self.ping.texture } else { &self.pong.texture }
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

fn layer_id_owner_key(layer_id: &manifold_core::LayerId) -> i64 {
    let mut hasher = DefaultHasher::new();
    layer_id.hash(&mut hasher);
    // Ensure non-zero and distinct from clip keys by setting high bit
    (hasher.finish() | (1 << 63)) as i64
}

/// Check if an effect slice has any enabled effects with non-zero amount.
/// Unity ref: CompositorStack.cs lines 965-974 — checks enabled && GetParam(0) > 0.
fn has_enabled_effects(effects: &[EffectInstance]) -> bool {
    for fx in effects {
        if fx.enabled
            && *fx.effect_type() != EffectTypeId::UNKNOWN
            && fx.param_values.first().copied().unwrap_or(0.0) > 0.0
        {
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
    /// Per-frame uniform sub-allocator — batches all blend uniform writes into
    /// a single `queue.write_buffer()` call, reducing wgpu staging overhead.
    uniform_arena: UniformArena,
    /// Effect chain processor (owns its own ping-pong for effect processing).
    effect_chain: EffectChain,
    /// Registry of all effect processors.
    effect_registry: EffectRegistry,
    /// Wet/dry lerp pipeline for effect group blending.
    wet_dry_lerp: WetDryLerpPipeline,
    /// ACES tonemapping pipeline. Matches Unity's CompositorStack.tonemapMaterial +
    /// tonemappedOutput. Applied as the final step after master effects.
    tonemap: TonemapPipeline,
    /// LED tap: dedicated copy of the pre-tonemap composite, populated when
    /// led_exit_index == 0. Avoids the main buffer being overwritten by tonemap
    /// and master effects before the LED pipeline reads it.
    led_tap: Option<crate::render_target::RenderTarget>,
}

impl LayerCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) -> Self {
        Self {
            main: PingPong::new(device, width, height, "Compositor"),
            layer_buf: None,
            blend: BlendResources::new(device, width, height),
            uniform_arena: UniformArena::new(device),
            effect_chain: EffectChain::new(),
            effect_registry: EffectRegistry::new(device, queue),
            wet_dry_lerp: WetDryLerpPipeline::new(device),
            tonemap: TonemapPipeline::new(device, width, height),
            led_tap: None,
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
        gpu: &mut GpuEncoder,
        input_view: &'a wgpu::TextureView,
        input_texture: &wgpu::Texture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> Option<&'a wgpu::TextureView> {
        effect_chain.apply_chain(
            gpu,
            registry,
            input_view,
            input_texture,
            effects,
            groups,
            ctx,
            Some(wet_dry_lerp),
            gpu_profiler,
        )
    }

    /// Clean up per-owner effect state for a stopped clip.
    pub fn cleanup_clip_owner(&mut self, clip_id: &str) {
        let owner_key = clip_id_owner_key(clip_id);
        self.effect_registry.cleanup_clip_owner(owner_key);
    }

    /// Composite all clips into main buffer, grouping by layer.
    fn composite(
        &mut self,
        gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();
        let aspect = width as f32 / height as f32;

        // Reset uniform arena for this frame — all blend uniform writes
        // will be batched into a single queue.write_buffer at frame end.
        self.uniform_arena.reset();

        // Clear main to opaque black
        self.main.clear_source(gpu.encoder, true);

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
            if let Some(ld) = layer_desc
                && (ld.is_muted || (any_solo && !ld.is_solo)) {
                    while i < clips.len() && clips[i].layer_index == layer_idx {
                        i += 1;
                    }
                    continue;
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
                .is_some_and(|ld| has_enabled_effects(ld.effects));

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
                        gpu,
                        clip.texture_view, clip.texture,
                        clip.effects, clip.effect_groups, &ctx,
                        gpu_profiler,
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
                    gpu, &mut self.uniform_arena,
                    self.main.source_view(),
                    effective_blend_view,
                    self.main.target_view(),
                    &uniforms,
                    gpu_profiler,
                );
                self.main.swap();
            } else {
                // Multi-clip: composite into layer buffer, then into main
                self.ensure_layer_buffers(gpu.device);
                let layer_buf = self.layer_buf.as_mut().unwrap();

                // Clear layer buffer to transparent
                layer_buf.clear_source(gpu.encoder, false);

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
                            gpu,
                            clip.texture_view, clip.texture,
                            clip.effects, clip.effect_groups, &ctx,
                            gpu_profiler,
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
                        gpu, &mut self.uniform_arena,
                        layer_buf.source_view(),
                        effective_blend_view,
                        layer_buf.target_view(),
                        &uniforms,
                        gpu_profiler,
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
                            owner_key: layer_desc.map_or(0, |ld| layer_id_owner_key(&ld.layer_id)),
                            is_clip_level: false, edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        let layer_buf = self.layer_buf.as_ref().unwrap();
                        Self::apply_effects(
                            &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                            gpu,
                            layer_buf.source_view(), layer_buf.source_texture(),
                            ld.effects, ld.effect_groups, &ctx,
                            gpu_profiler,
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
                    gpu, &mut self.uniform_arena,
                    self.main.source_view(),
                    effective_layer_view,
                    self.main.target_view(),
                    &uniforms,
                    gpu_profiler,
                );
                self.main.swap();
            }
        }

        // Master effects (bloom, halation, CRT) are applied AFTER tonemapping
        // in render() so their glow contribution extends above 1.0 for HDR displays.
        // On SDR displays, values > 1.0 clip to white — visually identical to pre-tonemap.
    }
}

impl Compositor for LayerCompositor {
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> &wgpu::TextureView {
        if frame.clips.is_empty() {
            // Unity: CompositorStack.cs returns immediately for empty playback.
            // Clear to black + return tonemap output (already cleared from previous frame).
            // Skips ALL master effects, tonemap, and LED tap — zero GPU draw calls.
            self.main.clear_source(gpu.encoder, true);
            self.tonemap.clear(gpu.encoder);
            return &self.tonemap.output.view;
        }

        self.composite(gpu, frame, gpu_profiler);

        // LED tap: capture pre-tonemap composite when exit index is 0.
        // main.source holds the all-layers composite at this point, before
        // tonemap and master effects overwrite it.
        if frame.led_exit_index == 0 {
            let (w, h) = (self.main.width(), self.main.height());
            let tap = self.led_tap.get_or_insert_with(|| {
                crate::render_target::RenderTarget::new(
                    gpu.device, w, h, wgpu::TextureFormat::Rgba16Float, "LED_Tap",
                )
            });
            if tap.width != w || tap.height != h {
                *tap = crate::render_target::RenderTarget::new(
                    gpu.device, w, h, wgpu::TextureFormat::Rgba16Float, "LED_Tap",
                );
            }
            gpu.encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: self.main.source_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &tap.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        } else {
            // Free the tap buffer when not needed
            self.led_tap = None;
        }

        // Tonemap the composited scene (before master glow effects).
        self.tonemap.apply(
            gpu,
            self.main.source_view(),
            &frame.tonemap,
            gpu_profiler,
        );

        // Apply master effects (bloom, halation, CRT) AFTER tonemapping.
        // Glow contribution pushes values > 1.0 for HDR/EDR displays.
        // On SDR displays, values > 1.0 clip to white — same visual result.
        //
        // The effect chain reads directly from tonemap.output (no copy into main)
        // and blits the processed result back to tonemap.output via Opaque blend.
        // Saves 2× full-resolution texture copies per frame.
        if has_enabled_effects(frame.master_effects) {
            let width = self.main.width();
            let height = self.main.height();

            let ctx = EffectContext {
                time: frame.time,
                beat: frame.beat,
                dt: frame.dt,
                width,
                height,
                owner_key: 0,
                is_clip_level: false,
                edge_stretch_width: 0.5625,
                frame_count: frame.frame_count as i64,
            };

            // Feed tonemap output directly into the effect chain — the first
            // effect reads from tonemap.output without copying.
            if let Some(_processed) = Self::apply_effects(
                &mut self.effect_chain, &mut self.effect_registry, &self.wet_dry_lerp,
                gpu,
                &self.tonemap.output.view, &self.tonemap.output.texture,
                frame.master_effects,
                frame.master_effect_groups, &ctx,
                gpu_profiler,
            ) {
                // Copy processed result back into tonemap output via GPU memcpy.
                // Replaces the old Opaque compute blend pass — same result, zero
                // shader cost. Unity ref: same pattern as Graphics.CopyTexture.
                gpu.encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: self.effect_chain.source_texture(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tonemap.output.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // Flush all accumulated blend uniforms to the GPU in a single write.
        self.uniform_arena.flush(gpu.device, gpu.queue);

        &self.tonemap.output.view
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.main.resize(device, width, height);
        if let Some(lb) = &mut self.layer_buf {
            lb.resize(device, width, height);
        }
        self.blend.resize(width, height);
        self.effect_chain.resize(device, width, height);
        self.effect_registry.resize_all(device, width, height);
        self.tonemap.resize(device, width, height);
        // LED tap will be recreated at new size on next frame if needed.
        self.led_tap = None;
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

    fn output_view(&self) -> &wgpu::TextureView {
        &self.tonemap.output.view
    }

    fn led_tap_view(&self) -> Option<&wgpu::TextureView> {
        self.led_tap.as_ref().map(|t| &t.view)
    }

    fn cleanup_clip_owner(&mut self, clip_id: &str) {
        self.cleanup_clip_owner(clip_id);
    }
}
