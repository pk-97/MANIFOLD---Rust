use crate::compositor::{Compositor, CompositorFrame};
use crate::effect::EffectContext;
use crate::effect_chain::EffectChain;
use crate::effect_registry::EffectRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::tonemap::TonemapPipeline;
use crate::uniform_arena::UniformArena;
use crate::wet_dry_lerp::WetDryLerpPipeline;
use ahash::AHashMap;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::{BlendMode, EffectTypeId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Descriptor for a single clip to composite.
pub struct CompositeClipDescriptor<'a> {
    pub clip_id: &'a str,
    pub texture: &'a GpuTexture,
    pub layer_index: i32,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub effects: &'a [EffectInstance],
    pub effect_groups: &'a [EffectGroup],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniforms {
    blend_mode: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<BlendUniforms>() == 16);

/// Blend WGSL source — shared across all specialized blend mode variants.
const BLEND_WGSL: &str = include_str!("generators/shaders/compositor_blend_compute.wgsl");

/// Number of blend modes (Normal=0 through Darken=12).
const BLEND_MODE_COUNT: u32 = 13;

/// GPU resources for blend operations using native Metal compute.
///
/// One specialized pipeline per blend mode — the Metal compiler dead-code
/// eliminates inactive switch branches in each variant.
/// Opaque (mode 6) further eliminates the base texture read.
struct BlendResources {
    /// Specialized pipelines indexed by blend mode.
    pipelines: AHashMap<u32, manifold_gpu::GpuComputePipeline>,
    sampler: manifold_gpu::GpuSampler,
    /// Compositor width/height — needed for dispatch_workgroups.
    width: u32,
    height: u32,
}

impl BlendResources {
    fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let mut pipelines = AHashMap::with_capacity(BLEND_MODE_COUNT as usize);
        for mode in 0..BLEND_MODE_COUNT {
            let label = format!("Blend Mode {mode}");
            let mode_str = format!("{mode}u");
            let pipeline = device.create_specialized_compute_pipeline(
                BLEND_WGSL,
                "cs_main",
                &[("u.blend_mode", &mode_str)],
                &label,
            );
            pipelines.insert(mode, pipeline);
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
            pipelines,
            sampler,
            width,
            height,
        }
    }

    /// Execute a compute blend. Selects the specialized pipeline for the blend mode.
    fn blend_pass(
        &self,
        gpu: &mut GpuEncoder,
        arena: &mut UniformArena,
        source_texture: &GpuTexture,
        blend_texture: &GpuTexture,
        target_texture: &GpuTexture,
        uniforms: &BlendUniforms,
    ) {
        let _offset = arena.push(uniforms);

        let pipeline = self
            .pipelines
            .get(&uniforms.blend_mode)
            .or_else(|| self.pipelines.get(&0))
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
            RenderTarget::new(
                device,
                width,
                height,
                format,
                &format!("{label_prefix} Ping"),
            )
        };
        let pong = if let Some(p) = pool {
            RenderTarget::new_pooled(p, width, height, format, &format!("{label_prefix} Pong"))
        } else {
            RenderTarget::new(
                device,
                width,
                height,
                format,
                &format!("{label_prefix} Pong"),
            )
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

/// Count active (non-muted, non-solo-hidden) layers in the frame.
fn count_active_layers(frame: &CompositorFrame) -> usize {
    let clips = frame.clips;
    let any_solo = frame.layers.iter().any(|l| l.is_solo);
    let mut count = 0;
    let mut i = 0;
    while i < clips.len() {
        let layer_idx = clips[i].layer_index;
        let layer_desc = frame.layers.iter().find(|l| l.layer_index == layer_idx);
        while i < clips.len() && clips[i].layer_index == layer_idx {
            i += 1;
        }
        if let Some(ld) = layer_desc
            && (ld.is_muted || (any_solo && !ld.is_solo))
        {
            continue;
        }
        count += 1;
    }
    count
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

/// Output descriptor for a single processed layer, ready for the blend pass.
///
/// Uses a raw pointer for the texture reference to avoid borrow checker conflicts
/// between `generate_layers()` (which borrows effect chain / layer buf textures)
/// and `blend_layers()` (which needs `&mut self` for the main ping-pong).
/// Safety: the texture pointer is valid for the duration of the frame — textures
/// are owned by effect chains, layer bufs, or clip render targets, none of which
/// are reallocated between generate and blend.
pub(crate) struct LayerOutput {
    /// Final texture for this layer (post-effects). Raw pointer to avoid lifetime.
    texture: *const GpuTexture,
    /// Layer blend mode.
    blend_mode: BlendMode,
    /// Layer opacity (includes per-clip opacity for single-clip layers).
    opacity: f32,
}

// Safety: LayerOutput is only used within the compositor on the content thread.
// The raw pointer points to GpuTexture owned by effect chains, layer bufs, or
// clip render targets that are valid for the frame duration. The Vec<LayerOutput>
// field on LayerCompositor makes the struct non-Send without this impl.
unsafe impl Send for LayerOutput {}

impl LayerOutput {
    fn texture(&self) -> &GpuTexture {
        // Safety: pointer is valid for the frame duration (see struct doc).
        unsafe { &*self.texture }
    }
}

/// Layer-aware compositor with per-layer ping-pong blending.
///
/// Compositing flow (two-phase):
/// 1. **generate_layers**: process each layer's clips + effects independently
/// 2. **blend_layers**: serial blend of all layer outputs into main accumulator
pub struct LayerCompositor {
    /// Main accumulation ping-pong (opaque black init).
    main: PingPong,
    /// Per-layer scratch buffers (lazy, transparent black init).
    /// One per active multi-clip layer, reused across frames.
    layer_bufs: Vec<PingPong>,
    /// GPU resources for blend operations (pipeline, sampler).
    blend: BlendResources,
    /// Per-frame uniform sub-allocator — batches all blend uniform writes into
    /// a single buffer. On native path, arena buffer is not read (uses inline
    /// set_bytes), but offset tracking is preserved.
    uniform_arena: UniformArena,
    /// Per-layer effect chain processors. Pool grows to match peak active layer
    /// count, then stays stable (no per-frame allocation after warmup).
    /// Index 0..N-1 for clip/layer effects, last entry reserved for master effects.
    effect_chains: Vec<EffectChain>,
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
    /// Pre-allocated scratch buffer for per-layer output descriptors.
    /// Cleared and populated each frame by generate_layers / composite_parallel
    /// to avoid per-frame heap allocation.
    layer_outputs_scratch: Vec<LayerOutput>,
    /// Shared event for async compute synchronization.
    /// Layer command buffers signal this with incrementing values;
    /// compositor command buffer waits for the final value.
    /// Created lazily on first parallel frame.
    #[cfg(target_os = "macos")]
    async_event: Option<manifold_gpu::GpuEvent>,
    /// Base signal value for the current frame's async compute.
    /// Each layer signals base + layer_index; compositor waits for base + layer_count.
    #[cfg(target_os = "macos")]
    async_signal_base: u64,
    /// How many layer_bufs were actually used last frame.
    last_layer_buf_used: usize,
    /// How many effect_chains were actually used last frame.
    last_effect_chain_used: usize,
}

impl LayerCompositor {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        Self {
            main: PingPong::new(device, None, width, height, "Compositor"),
            layer_bufs: Vec::new(),
            blend: BlendResources::new(device, width, height),
            uniform_arena: UniformArena::new(device),
            effect_chains: Vec::new(),
            effect_registry: EffectRegistry::new(device),
            wet_dry_lerp: WetDryLerpPipeline::new(device),
            tonemap: TonemapPipeline::new(device, width, height),
            led_tap: None,
            layer_outputs_scratch: Vec::new(),
            #[cfg(target_os = "macos")]
            async_event: None,
            #[cfg(target_os = "macos")]
            async_signal_base: 0,
            last_layer_buf_used: 0,
            last_effect_chain_used: 0,
        }
    }

    /// Ensure we have at least `count` layer scratch buffers available.
    fn ensure_layer_bufs(
        &mut self,
        count: usize,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
    ) {
        let w = self.main.width();
        let h = self.main.height();
        while self.layer_bufs.len() < count {
            let idx = self.layer_bufs.len();
            self.layer_bufs.push(PingPong::new(
                device,
                pool,
                w,
                h,
                &format!("Layer Scratch {idx}"),
            ));
        }
    }

    /// Ensure we have at least `count` effect chains available.
    fn ensure_effect_chains(&mut self, count: usize) {
        while self.effect_chains.len() < count {
            self.effect_chains.push(EffectChain::new());
        }
    }

    /// Trim oversized layer_bufs and effect_chains down to actual usage + headroom.
    /// Excess textures are dropped immediately (freed by Metal) rather than released
    /// to the TexturePool — compositor scratch buffers are large and rarely re-needed,
    /// so pooling them just delays the memory savings.
    /// Headroom of 2 prevents oscillation if usage fluctuates frame-to-frame.
    fn trim_excess_buffers(&mut self) {
        const HEADROOM: usize = 2;

        let target_layer_bufs = self.last_layer_buf_used.saturating_add(HEADROOM);
        if self.layer_bufs.len() > target_layer_bufs {
            self.layer_bufs.truncate(target_layer_bufs);
        }

        let target_effect_chains = self.last_effect_chain_used.saturating_add(HEADROOM);
        if self.effect_chains.len() > target_effect_chains {
            self.effect_chains.truncate(target_effect_chains);
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

    /// Phase A: Process each layer's clips + effects into per-layer output textures.
    ///
    /// For single-clip layers without layer effects, the output is the clip texture
    /// (possibly post-clip-effects via the layer's effect chain).
    /// For multi-clip layers or layers with effects, clips are composited into a
    /// per-layer scratch buffer and layer effects are applied.
    ///
    /// Each layer uses its own effect chain (no shared state between layers).
    /// Populates `self.layer_outputs_scratch` for the blend pass.
    fn generate_layers(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();

        // Check for any solo layer
        let any_solo = frame.layers.iter().any(|l| l.is_solo);

        // Count active layers for pool sizing
        let mut active_layer_count = 0usize;
        let mut multi_clip_layer_count = 0usize;
        {
            let mut ci = 0;
            while ci < clips.len() {
                let layer_idx = clips[ci].layer_index;
                let layer_desc = frame.layers.iter().find(|l| l.layer_index == layer_idx);
                let start = ci;
                while ci < clips.len() && clips[ci].layer_index == layer_idx {
                    ci += 1;
                }
                if let Some(ld) = layer_desc
                    && (ld.is_muted || (any_solo && !ld.is_solo))
                {
                    continue;
                }
                let clip_count = ci - start;
                let has_layer_effects =
                    layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));
                active_layer_count += 1;
                if clip_count > 1 || has_layer_effects {
                    multi_clip_layer_count += 1;
                }
            }
        }

        // Ensure enough effect chains and scratch buffers
        self.ensure_effect_chains(active_layer_count);
        if multi_clip_layer_count > 0 {
            self.ensure_layer_bufs(multi_clip_layer_count, gpu.device, gpu.pool);
        }

        self.layer_outputs_scratch.clear();
        let mut effect_chain_idx = 0usize;
        let mut layer_buf_idx = 0usize;

        // Raw pointers to avoid borrow checker conflicts between per-layer
        // effect chains / layer bufs and the rest of self. Each iteration
        // accesses a unique index — no aliasing.
        let effect_chains_ptr = self.effect_chains.as_mut_ptr();
        let layer_bufs_ptr = self.layer_bufs.as_mut_ptr();

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
            let has_layer_effects = layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

            // Acquire this layer's effect chain (unique index per layer).
            // Safety: ec_idx is unique per iteration and < effect_chains.len().
            let ec_idx = effect_chain_idx;
            effect_chain_idx += 1;
            let effect_chain = unsafe { &mut *effect_chains_ptr.add(ec_idx) };

            if group.len() == 1 && !has_layer_effects {
                // Single clip with NO layer effects — pass texture straight through
                let clip = &group[0];

                self.layer_outputs_scratch.push(LayerOutput {
                    texture: clip.texture,
                    blend_mode: layer_blend,
                    opacity: layer_opacity * clip.opacity,
                });
            } else {
                // Multi-clip or layer-effects: composite into layer buffer
                // Safety: lb_idx is unique per multi-clip layer and < layer_bufs.len().
                let lb_idx = layer_buf_idx;
                layer_buf_idx += 1;
                let layer_buf = unsafe { &mut *layer_bufs_ptr.add(lb_idx) };

                // Clear layer buffer to transparent
                layer_buf.clear_source(gpu, false);

                // Composite each clip into layer buffer with Normal blend
                for clip in group {
                    let uniforms = BlendUniforms {
                        blend_mode: BlendMode::Normal as u32,
                        opacity: clip.opacity,
                        _pad0: 0,
                        _pad1: 0,
                    };
                    self.blend.blend_pass(
                        gpu,
                        &mut self.uniform_arena,
                        layer_buf.source_texture(),
                        clip.texture,
                        layer_buf.target_texture(),
                        &uniforms,
                    );
                    layer_buf.swap();
                }

                // Apply layer-level effects to composited layer buffer
                let layer_source = if let Some(ld) = layer_desc
                    && has_enabled_effects(ld.effects)
                {
                    let ctx = EffectContext {
                        time: frame.time,
                        beat: frame.beat,
                        dt: frame.dt,
                        width,
                        height,
                        output_width: frame.output_width,
                        output_height: frame.output_height,
                        owner_key: layer_desc.map_or(0, |ld| layer_id_owner_key(&ld.layer_id)),
                        is_clip_level: false,
                        edge_stretch_width: 0.5625,
                        frame_count: frame.frame_count as i64,
                    };
                    Self::apply_effects(
                        effect_chain,
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
                };

                let effective_layer_tex: *const GpuTexture =
                    layer_source.unwrap_or(layer_buf.source_texture());

                self.layer_outputs_scratch.push(LayerOutput {
                    texture: effective_layer_tex,
                    blend_mode: layer_blend,
                    opacity: layer_opacity,
                });
            }
        }

        // Record actual usage for trim_excess_buffers.
        self.last_layer_buf_used = layer_buf_idx;
        self.last_effect_chain_used = effect_chain_idx;
    }

    /// Phase B: Blend all layer outputs into main in order.
    ///
    /// Layers are blended bottom-to-top (order preserved from generate_layers).
    /// This pass is always serial — each blend reads the previous blend's result.
    fn blend_layers(&mut self, gpu: &mut GpuEncoder, layer_outputs: &[LayerOutput]) {
        // Clear main to opaque black
        self.main.clear_source(gpu, true);

        for output in layer_outputs {
            let uniforms = BlendUniforms {
                blend_mode: output.blend_mode as u32,
                opacity: output.opacity,
                _pad0: 0,
                _pad1: 0,
            };
            self.blend.blend_pass(
                gpu,
                &mut self.uniform_arena,
                self.main.source_texture(),
                output.texture(),
                self.main.target_texture(),
                &uniforms,
            );
            self.main.swap();
        }
    }

    /// Serial composite path: single encoder for all work.
    /// Used when only 1 active layer (no parallel benefit).
    fn composite_serial(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) {
        self.uniform_arena.reset();
        self.generate_layers(gpu, frame);
        // Safety: layer_outputs_scratch contains raw pointers to textures owned
        // by effect chains, layer bufs, or clip render targets — all valid for
        // the frame duration. Using a raw pointer avoids a split-borrow conflict
        // with blend_layers (which also needs &mut self for main ping-pong).
        let outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let outputs_len = self.layer_outputs_scratch.len();
        let outputs = unsafe { std::slice::from_raw_parts(outputs_ptr, outputs_len) };
        self.blend_layers(gpu, outputs);
    }

    /// Parallel composite path: one command buffer per layer for generation,
    /// then a serial blend on the original command buffer.
    ///
    /// Each layer's generation encodes into its own MTLCommandBuffer, signals
    /// a GpuEvent, and commits. The GPU schedules these for concurrent execution.
    /// The original command buffer (passed as `compositor_gpu`) waits on all
    /// layer completions before blending.
    ///
    /// Safety: per-layer effect chains and scratch buffers use raw pointer access
    /// (unique index per layer, no aliasing). LayerOutput textures are valid for
    /// the frame duration since they're owned by effect chains, layer bufs, or
    /// clip render targets that aren't reallocated between generate and blend.
    #[cfg(target_os = "macos")]
    fn composite_parallel(&mut self, compositor_gpu: &mut GpuEncoder, frame: &CompositorFrame) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();

        let device = compositor_gpu.device;
        let pool = compositor_gpu.pool;

        self.uniform_arena.reset();

        // Ensure async event exists
        if self.async_event.is_none() {
            self.async_event = Some(device.create_event());
        }
        // Raw pointer to avoid borrow conflict with &mut self later.
        // Safety: async_event lives for the duration of this method and
        // is not modified (only signal values change, which is interior mutation).
        let async_event: *const manifold_gpu::GpuEvent = self.async_event.as_ref().unwrap();

        // Check for any solo layer
        let any_solo = frame.layers.iter().any(|l| l.is_solo);

        // Pre-scan: count active layers and multi-clip layers for pool sizing.
        let mut active_layer_count = 0usize;
        let mut multi_clip_layer_count = 0usize;
        {
            let mut ci = 0;
            while ci < clips.len() {
                let layer_idx = clips[ci].layer_index;
                let layer_desc = frame.layers.iter().find(|l| l.layer_index == layer_idx);
                let start = ci;
                while ci < clips.len() && clips[ci].layer_index == layer_idx {
                    ci += 1;
                }
                if let Some(ld) = layer_desc
                    && (ld.is_muted || (any_solo && !ld.is_solo))
                {
                    continue;
                }
                let clip_count = ci - start;
                let has_layer_effects =
                    layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));
                active_layer_count += 1;
                if clip_count > 1 || has_layer_effects {
                    multi_clip_layer_count += 1;
                }
            }
        }

        self.ensure_effect_chains(active_layer_count);
        if multi_clip_layer_count > 0 {
            self.ensure_layer_bufs(multi_clip_layer_count, device, pool);
        }

        let effect_chains_ptr = self.effect_chains.as_mut_ptr();
        let layer_bufs_ptr = self.layer_bufs.as_mut_ptr();
        let base_signal = self.async_signal_base;

        self.layer_outputs_scratch.clear();
        let mut effect_chain_idx = 0usize;
        let mut layer_buf_idx = 0usize;
        let mut layer_signal_idx = 0u64;

        // Process each layer on its own command buffer.
        let mut i = 0;
        while i < clips.len() {
            let layer_idx = clips[i].layer_index;
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

            let group_start = i;
            while i < clips.len() && clips[i].layer_index == layer_idx {
                i += 1;
            }
            let group = &clips[group_start..i];

            let layer_blend = layer_desc.map_or(BlendMode::Normal, |l| l.blend_mode);
            let layer_opacity = layer_desc.map_or(1.0, |l| l.opacity);
            let has_layer_effects = layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

            let ec_idx = effect_chain_idx;
            effect_chain_idx += 1;
            let effect_chain = unsafe { &mut *effect_chains_ptr.add(ec_idx) };

            // Create per-layer command buffer
            let mut layer_enc = device.create_encoder("Layer");

            // Scope the GpuEncoder wrapper so it drops before signal+commit
            {
                let mut gpu = if let Some(p) = pool {
                    GpuEncoder::with_pool(&mut layer_enc, device, p)
                } else {
                    GpuEncoder::new(&mut layer_enc, device)
                };

                if group.len() == 1 && !has_layer_effects {
                    // Single clip with NO layer effects — pass texture straight through
                    let clip = &group[0];

                    self.layer_outputs_scratch.push(LayerOutput {
                        texture: clip.texture,
                        blend_mode: layer_blend,
                        opacity: layer_opacity * clip.opacity,
                    });
                } else {
                    let lb_idx = layer_buf_idx;
                    layer_buf_idx += 1;
                    let layer_buf = unsafe { &mut *layer_bufs_ptr.add(lb_idx) };

                    layer_buf.clear_source(&mut gpu, false);

                    for clip in group {
                        let uniforms = BlendUniforms {
                            blend_mode: BlendMode::Normal as u32,
                            opacity: clip.opacity,
                            _pad0: 0,
                            _pad1: 0,
                        };
                        self.blend.blend_pass(
                            &mut gpu,
                            &mut self.uniform_arena,
                            layer_buf.source_texture(),
                            clip.texture,
                            layer_buf.target_texture(),
                            &uniforms,
                        );
                        layer_buf.swap();
                    }

                    // Layer-level effects
                    let layer_source = if let Some(ld) = layer_desc
                        && has_enabled_effects(ld.effects)
                    {
                        let ctx = EffectContext {
                            time: frame.time,
                            beat: frame.beat,
                            dt: frame.dt,
                            width,
                            height,
                            output_width: frame.output_width,
                            output_height: frame.output_height,
                            owner_key: layer_desc.map_or(0, |ld| layer_id_owner_key(&ld.layer_id)),
                            is_clip_level: false,
                            edge_stretch_width: 0.5625,
                            frame_count: frame.frame_count as i64,
                        };
                        Self::apply_effects(
                            effect_chain,
                            &mut self.effect_registry,
                            &self.wet_dry_lerp,
                            &mut gpu,
                            layer_buf.source_texture(),
                            ld.effects,
                            ld.effect_groups,
                            &ctx,
                        )
                    } else {
                        None
                    };

                    let effective_layer_tex: *const GpuTexture =
                        layer_source.unwrap_or(layer_buf.source_texture());

                    self.layer_outputs_scratch.push(LayerOutput {
                        texture: effective_layer_tex,
                        blend_mode: layer_blend,
                        opacity: layer_opacity,
                    });
                }
            } // gpu wrapper drops here, releasing borrow on layer_enc

            // Signal completion for this layer and commit
            layer_signal_idx += 1;
            let signal_value = base_signal + layer_signal_idx;
            layer_enc.signal_event_value(unsafe { &*async_event }, signal_value);
            layer_enc.commit();
        }

        // Record actual usage for trim_excess_buffers.
        self.last_layer_buf_used = layer_buf_idx;
        self.last_effect_chain_used = effect_chain_idx;

        // Update base for next frame
        self.async_signal_base = base_signal + layer_signal_idx;

        // Compositor command buffer waits for all layer completions
        let final_signal = base_signal + layer_signal_idx;
        if layer_signal_idx > 0 {
            compositor_gpu
                .native_enc
                .wait_event(unsafe { &*async_event }, final_signal);
        }

        // Serial blend phase on the compositor command buffer.
        // Safety: layer_outputs_scratch is populated above and not modified during blend.
        let outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let outputs_len = self.layer_outputs_scratch.len();
        let outputs = unsafe { std::slice::from_raw_parts(outputs_ptr, outputs_len) };
        self.blend_layers(compositor_gpu, outputs);
    }
}

impl Compositor for LayerCompositor {
    fn render(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) -> &GpuTexture {
        if frame.clips.is_empty() {
            // Unity: CompositorStack.cs returns immediately for empty playback.
            // Clear to black + return tonemap output (already cleared from previous frame).
            // Skips ALL master effects, tonemap, and LED tap — zero GPU draw calls.
            gpu.clear_texture(self.main.source_texture(), 0.0, 0.0, 0.0, 1.0);
            self.tonemap.clear(gpu);
            // No layers active — trim excess buffers from previous frames.
            self.last_layer_buf_used = 0;
            self.last_effect_chain_used = 0;
            self.trim_excess_buffers();
            return &self.tonemap.output.texture;
        }

        // Choose serial vs parallel composite path.
        // Parallel path creates per-layer command buffers for GPU-concurrent
        // generation. Only activated with 2+ active layers (no overhead for
        // single-layer frames).
        #[cfg(target_os = "macos")]
        {
            let active_layers = count_active_layers(frame);
            if active_layers >= 2 {
                self.composite_parallel(gpu, frame);
            } else {
                self.composite_serial(gpu, frame);
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.composite_serial(gpu, frame);

        // LED tap: capture pre-tonemap composite when exit index is 0.
        // main.source holds the all-layers composite at this point, before
        // tonemap and master effects overwrite it.
        if frame.led_exit_index == 0 {
            let (w, h) = (self.main.width(), self.main.height());
            let tap = self.led_tap.get_or_insert_with(|| {
                RenderTarget::new(gpu.device, w, h, GpuTextureFormat::Rgba16Float, "LED_Tap")
            });
            if tap.width != w || tap.height != h {
                *tap =
                    RenderTarget::new(gpu.device, w, h, GpuTextureFormat::Rgba16Float, "LED_Tap");
            }
            gpu.copy_texture_to_texture(self.main.source_texture(), &tap.texture, w, h);
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
                output_width: frame.output_width,
                output_height: frame.output_height,
                owner_key: 0,
                is_clip_level: false,
                edge_stretch_width: 0.5625,
                frame_count: frame.frame_count as i64,
            };

            // Use a dedicated effect chain for master effects (index 0 in the
            // pool — always available since we ensure at least 1 chain exists).
            self.ensure_effect_chains(1);
            let master_ec = &mut self.effect_chains[0];

            // Feed tonemap output directly into the effect chain — the first
            // effect reads from tonemap.output without copying.
            if let Some(_processed) = Self::apply_effects(
                master_ec,
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
                    master_ec.source_texture_pub(),
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

        // Trim oversized buffer pools. Excess textures released to TexturePool
        // for recycling (or dropped if no pool). Headroom of +2 prevents
        // oscillation when layer count fluctuates frame-to-frame.
        self.trim_excess_buffers();

        &self.tonemap.output.texture
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.main.resize(device, width, height);
        for lb in &mut self.layer_bufs {
            lb.resize(device, width, height);
        }
        self.blend.resize(width, height);
        for ec in &mut self.effect_chains {
            ec.resize(device, width, height);
        }
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

    fn clear_all_effect_state(&mut self) {
        self.effect_registry.clear_all_state();
    }

    fn flush_all_background_work(&mut self) {
        self.effect_registry.flush_all_background_work();
    }

    fn led_tap_texture(&self) -> Option<&GpuTexture> {
        self.led_tap.as_ref().map(|t| &t.texture)
    }
}
