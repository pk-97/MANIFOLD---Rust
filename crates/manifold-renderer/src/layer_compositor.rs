use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use ahash::AHashMap;
use manifold_core::{BlendMode, EffectTypeId};
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
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
    pub texture: &'a GpuTexture,
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

/// Blend WGSL source — shared across all specialized blend mode variants.
const BLEND_WGSL: &str = include_str!("generators/shaders/compositor_blend_compute.wgsl");

/// Number of blend modes (Normal=0 through Darken=12).
const BLEND_MODE_COUNT: u32 = 13;

/// GPU resources for blend operations using native Metal compute.
/// Holds one specialized pipeline per blend mode — Metal compiler dead-code
/// eliminates inactive switch branches in each variant.
struct BlendResources {
    /// Specialized pipelines indexed by blend mode (0..12).
    blend_pipelines: AHashMap<u32, manifold_gpu::GpuComputePipeline>,
    sampler: manifold_gpu::GpuSampler,
    /// Compositor width/height — needed for dispatch_workgroups.
    width: u32,
    height: u32,
}

impl BlendResources {
    fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let mut blend_pipelines = AHashMap::with_capacity(BLEND_MODE_COUNT as usize);
        for mode in 0..BLEND_MODE_COUNT {
            let label = format!("Blend Mode {mode}");
            let mode_str = format!("{mode}u");
            let pipeline = device.create_specialized_compute_pipeline(
                BLEND_WGSL,
                "cs_main",
                &[("u.blend_mode", &mode_str)],
                &label,
            );
            blend_pipelines.insert(mode, pipeline);
        }

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Nearest,
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
        });

        Self {
            blend_pipelines,
            sampler,
            width,
            height,
        }
    }

    /// Execute a compute blend: reads source + blend textures, writes to target storage texture.
    /// Selects the specialized pipeline matching the blend mode in uniforms.
    fn blend_pass(
        &self,
        gpu: &mut GpuEncoder,
        arena: &mut UniformArena,
        source_texture: &GpuTexture,
        blend_texture: &GpuTexture,
        target_texture: &GpuTexture,
        uniforms: &BlendUniforms,
    ) {
        // Push to arena for offset tracking (arena buffer not read on native path)
        let _offset = arena.push(uniforms);

        // Select specialized pipeline for this blend mode (fall back to mode 0 if unknown)
        let pipeline = self.blend_pipelines.get(&uniforms.blend_mode)
            .or_else(|| self.blend_pipelines.get(&0))
            .unwrap();

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: source_texture,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: blend_texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target_texture,
                },
            ],
            [self.width.div_ceil(16), self.height.div_ceil(16), 1],
            "Blend Pass",
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
    fn new(
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        width: u32,
        height: u32,
        label_prefix: &str,
    ) -> Self {
        let format = GpuTextureFormat::Rgba16Float;
        let ping = if let Some(p) = pool {
            RenderTarget::new_pooled(p, width, height, format, &format!("{label_prefix} Ping"))
        } else {
            RenderTarget::new(device, width, height, format, &format!("{label_prefix} Ping"))
        };
        let pong = if let Some(p) = pool {
            RenderTarget::new_pooled(p, width, height, format, &format!("{label_prefix} Pong"))
        } else {
            RenderTarget::new(device, width, height, format, &format!("{label_prefix} Pong"))
        };
        Self {
            ping,
            pong,
            use_ping_as_source: true,
        }
    }

    fn source_texture(&self) -> &GpuTexture {
        if self.use_ping_as_source {
            &self.ping.texture
        } else {
            &self.pong.texture
        }
    }

    fn target_texture(&self) -> &GpuTexture {
        if self.use_ping_as_source {
            &self.pong.texture
        } else {
            &self.ping.texture
        }
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.ping.resize(device, width, height);
        self.pong.resize(device, width, height);
    }

    fn width(&self) -> u32 {
        self.ping.width
    }
    fn height(&self) -> u32 {
        self.ping.height
    }

    /// Clear source buffer via native encoder.
    /// `opaque` = true clears to opaque black (a=1), false clears to transparent black (a=0).
    fn clear_source(&self, gpu: &mut GpuEncoder, opaque: bool) {
        if opaque {
            gpu.clear_texture(self.source_texture(), 0.0, 0.0, 0.0, 1.0);
        } else {
            gpu.clear_texture(self.source_texture(), 0.0, 0.0, 0.0, 0.0);
        }
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
    /// GPU resources for blend operations (pipeline, sampler).
    blend: BlendResources,
    /// Per-frame uniform sub-allocator — batches all blend uniform writes into
    /// a single buffer. On native path, arena buffer is not read (uses inline
    /// set_bytes), but offset tracking is preserved.
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
    led_tap: Option<RenderTarget>,
}

impl LayerCompositor {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        Self {
            main: PingPong::new(device, None, width, height, "Compositor"),
            layer_buf: None,
            blend: BlendResources::new(device, width, height),
            uniform_arena: UniformArena::new(device),
            effect_chain: EffectChain::new(),
            effect_registry: EffectRegistry::new(device),
            wet_dry_lerp: WetDryLerpPipeline::new(device),
            tonemap: TonemapPipeline::new(device, width, height),
            led_tap: None,
        }
    }

    /// Ensure lazy layer scratch buffers exist.
    fn ensure_layer_buffers(&mut self, device: &GpuDevice) {
        if self.layer_buf.is_none() {
            let w = self.main.width();
            let h = self.main.height();
            self.layer_buf = Some(PingPong::new(device, None, w, h, "Layer Scratch"));
        }
    }

    /// Apply effect chain to the given input texture, returning the processed texture
    /// if any effects were applied, or None if the input should be used as-is.
    fn apply_effects<'a>(
        effect_chain: &'a mut EffectChain,
        registry: &mut EffectRegistry,
        wet_dry_lerp: &WetDryLerpPipeline,
        gpu: &mut GpuEncoder,
        input_texture: &'a GpuTexture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> Option<&'a GpuTexture> {
        effect_chain.apply_chain(
            gpu,
            registry,
            input_texture,
            effects,
            groups,
            ctx,
            Some(wet_dry_lerp),
        )
    }

    /// Clean up per-owner effect state for a stopped clip.
    pub fn cleanup_clip_owner_internal(&mut self, clip_id: &str) {
        let owner_key = clip_id_owner_key(clip_id);
        self.effect_registry.cleanup_clip_owner(owner_key);
    }

    /// Composite all clips into main buffer, grouping by layer.
    fn composite(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();
        let aspect = width as f32 / height as f32;

        // Reset uniform arena for this frame
        self.uniform_arena.reset();
        // Clear main to opaque black
        self.main.clear_source(gpu, true);

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
                && (ld.is_muted || (any_solo && !ld.is_solo))
            {
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

            // Check if this layer has layer-level effects
            let has_layer_effects =
                layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

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
                        is_clip_level: true,
                        edge_stretch_width: 0.5625,
                        frame_count: frame.frame_count as i64,
                    };
                    Self::apply_effects(
                        &mut self.effect_chain,
                        &mut self.effect_registry,
                        &self.wet_dry_lerp,
                        gpu,
                        clip.texture,
                        clip.effects,
                        clip.effect_groups,
                        &ctx,
                    )
                } else {
                    None
                };
                let effective_blend_tex = blend_input.unwrap_or(clip.texture);

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
                    gpu,
                    &mut self.uniform_arena,
                    self.main.source_texture(),
                    effective_blend_tex,
                    self.main.target_texture(),
                    &uniforms,
                );
                self.main.swap();
            } else {
                // Multi-clip or layer-effects: composite into layer buffer, then into main
                self.ensure_layer_buffers(gpu.device);
                let layer_buf = self.layer_buf.as_mut().unwrap();

                // Clear layer buffer to transparent
                layer_buf.clear_source(gpu, false);

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
                            is_clip_level: true,
                            edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        Self::apply_effects(
                            &mut self.effect_chain,
                            &mut self.effect_registry,
                            &self.wet_dry_lerp,
                            gpu,
                            clip.texture,
                            clip.effects,
                            clip.effect_groups,
                            &ctx,
                        )
                    } else {
                        None
                    };
                    let effective_blend_tex = blend_input.unwrap_or(clip.texture);

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
                        gpu,
                        &mut self.uniform_arena,
                        layer_buf.source_texture(),
                        effective_blend_tex,
                        layer_buf.target_texture(),
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
                            owner_key: layer_desc
                                .map_or(0, |ld| layer_id_owner_key(&ld.layer_id)),
                            is_clip_level: false,
                            edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        let layer_buf = self.layer_buf.as_ref().unwrap();
                        Self::apply_effects(
                            &mut self.effect_chain,
                            &mut self.effect_registry,
                            &self.wet_dry_lerp,
                            gpu,
                            layer_buf.source_texture(),
                            ld.effects,
                            ld.effect_groups,
                            &ctx,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                let layer_buf = self.layer_buf.as_ref().unwrap();
                let effective_layer_tex =
                    layer_source.unwrap_or(layer_buf.source_texture());

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
                    gpu,
                    &mut self.uniform_arena,
                    self.main.source_texture(),
                    effective_layer_tex,
                    self.main.target_texture(),
                    &uniforms,
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
    ) -> &GpuTexture {
        if frame.clips.is_empty() {
            // Unity: CompositorStack.cs returns immediately for empty playback.
            // Clear to black + return tonemap output (already cleared from previous frame).
            // Skips ALL master effects, tonemap, and LED tap — zero GPU draw calls.
            gpu.clear_texture(self.main.source_texture(), 0.0, 0.0, 0.0, 1.0);
            self.tonemap.clear(gpu);
            return &self.tonemap.output.texture;
        }

        self.composite(gpu, frame);

        // LED tap: capture pre-tonemap composite when exit index is 0.
        // main.source holds the all-layers composite at this point, before
        // tonemap and master effects overwrite it.
        if frame.led_exit_index == 0 {
            let (w, h) = (self.main.width(), self.main.height());
            let tap = self.led_tap.get_or_insert_with(|| {
                RenderTarget::new(
                    gpu.device,
                    w,
                    h,
                    GpuTextureFormat::Rgba16Float,
                    "LED_Tap",
                )
            });
            if tap.width != w || tap.height != h {
                *tap = RenderTarget::new(
                    gpu.device,
                    w,
                    h,
                    GpuTextureFormat::Rgba16Float,
                    "LED_Tap",
                );
            }
            gpu.copy_texture_to_texture(
                self.main.source_texture(),
                &tap.texture,
                w,
                h,
            );
        } else {
            // Free the tap buffer when not needed
            self.led_tap = None;
        }

        // Tonemap the composited scene (before master glow effects).
        self.tonemap
            .apply(gpu, self.main.source_texture(), &frame.tonemap);

        // Apply master effects (bloom, halation, CRT) AFTER tonemapping.
        // Glow contribution pushes values > 1.0 for HDR/EDR displays.
        // On SDR displays, values > 1.0 clip to white — same visual result.
        //
        // The effect chain reads directly from tonemap.output (no copy into main)
        // and blits the processed result back to tonemap.output via copy.
        // Saves 2x full-resolution texture copies per frame.
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
                &mut self.effect_chain,
                &mut self.effect_registry,
                &self.wet_dry_lerp,
                gpu,
                &self.tonemap.output.texture,
                frame.master_effects,
                frame.master_effect_groups,
                &ctx,
            ) {
                // Copy processed result back into tonemap output via GPU memcpy.
                gpu.copy_texture_to_texture(
                    self.effect_chain.source_texture_pub(),
                    &self.tonemap.output.texture,
                    width,
                    height,
                );
            }
        }

        // Flush uniform arena (recreates buffer if capacity grew).
        // On native path, arena buffer is not read by GPU dispatches (uses inline
        // set_bytes), but we still flush to handle capacity growth.
        self.uniform_arena.flush(gpu.device);

        &self.tonemap.output.texture
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
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

    fn pre_tonemap_output(&self) -> &GpuTexture {
        self.main.source_texture()
    }

    fn output_texture(&self) -> &GpuTexture {
        &self.tonemap.output.texture
    }

    fn cleanup_clip_owner(&mut self, clip_id: &str) {
        self.cleanup_clip_owner_internal(clip_id);
    }

    fn led_tap_texture(&self) -> Option<&GpuTexture> {
        self.led_tap.as_ref().map(|t| &t.texture)
    }
}
