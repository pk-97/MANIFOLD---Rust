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
use manifold_core::{BlendMode, EffectTypeId, LayerId};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
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
            compare: None,
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

fn group_id_owner_key(layer_id: &manifold_core::LayerId) -> i64 {
    let mut hasher = DefaultHasher::new();
    layer_id.hash(&mut hasher);
    // Bit 62 for groups (bit 63 is used for layers)
    (hasher.finish() | (1 << 62)) as i64
}

/// Count active (non-muted, non-solo-hidden) layers in the frame.
fn count_active_layers(frame: &CompositorFrame, any_solo: bool) -> usize {
    let clips = frame.clips;
    let mut count = 0;
    let mut i = 0;
    while i < clips.len() {
        let layer_idx = clips[i].layer_index;
        let layer_desc = frame.find_layer(layer_idx);
        while i < clips.len() && clips[i].layer_index == layer_idx {
            i += 1;
        }
        if let Some(ld) = layer_desc
            && (ld.is_muted || (any_solo && !ld.is_solo) || ld.opacity <= 0.0)
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
            && fx.param_values.first().map(|p| p.value).unwrap_or(0.0) > 0.0
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
    /// Source layer index (for group folding — correlates with layer descriptors).
    layer_index: i32,
    /// Whether this layer should also be composited into the LED output buffer.
    blit_to_led: bool,
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
    /// Per-layer effect chain processors, indexed by `layer_idx`. Each
    /// chain stays bound to its layer for the lifetime of the project,
    /// so its cached `ChainGraph` (primitive instances + state)
    /// survives across frames even as clips fire/end. Inactive layers'
    /// slots stay empty (no `ChainGraph` cached). Pool grows to match
    /// the highest active `layer_idx`, then stabilises.
    effect_chains: Vec<EffectChain>,
    /// Dedicated effect chain for the post-blend master FX pass.
    /// Kept SEPARATE from `effect_chains` because the master pass'
    /// effects are completely unrelated to any layer's effects —
    /// sharing `effect_chains[0]` (which is layer 0's slot under
    /// the layer-idx-indexed scheme) would force a `ChainGraph`
    /// rebuild every frame when layer 0 has its own effects, and
    /// each rebuild wipes primitive state (Bloom mips, etc.) and
    /// re-runs the first-evaluate allocation path on the GPU. The
    /// dedicated chain costs ~56 bytes of idle struct space when
    /// unused and zero CPU when no master effects are present.
    master_effect_chain: EffectChain,
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
    /// Per-group scratch buffers (lazy, transparent black init).
    /// One per active group — each group needs its own buffer because
    /// LayerOutput raw pointers must remain valid until blend_layers.
    group_bufs: Vec<PingPong>,
    /// Per-group effect chains for group-level effects.
    group_effect_chains: Vec<EffectChain>,
    /// How many group_bufs / group_effect_chains were actually used last frame.
    last_group_buf_used: usize,
    /// Pre-allocated scratch for child layer indices during group folding.
    group_child_indices: Vec<i32>,
    /// Pre-allocated scratch for child output positions during group folding.
    group_child_positions: Vec<usize>,

    // ── LED per-layer routing ──
    /// Accumulation buffer for layers flagged `blit_to_led`. Lazily allocated on
    /// the first frame any LED layer is active; persistent across frames. When
    /// no LED layers exist, this is freed along with `led_tonemap`.
    led_main: Option<PingPong>,
    /// Dedicated tonemap pipeline for the LED composite. Distinct output from
    /// the main tonemap so both can be read independently.
    led_tonemap: Option<TonemapPipeline>,
    /// Dedicated effect chain for LED master FX. Stored as a standalone field
    /// (not in `effect_chains` Vec) so the shared resize path doesn't force it
    /// to full resolution — the LED chain auto-allocates at half-res via
    /// `ensure_buffers` driven by the LED EffectContext.
    /// Uses owner_key `LED_MASTER_OWNER_KEY` to keep temporal state separate
    /// from the main master chain.
    led_master_ec: Option<EffectChain>,

    /// Per-group LED scratch buffers, sized at LED grid resolution. One per
    /// group whose LED-flagged children need to flow through that group's
    /// effects on the LED path. Reused across frames; reallocated only on
    /// LED-grid dimension changes.
    led_group_bufs: Vec<PingPong>,
    /// Per-group LED effect chains. Distinct from `group_effect_chains` so
    /// temporal state on the LED path doesn't bleed into the screen path
    /// (and vice versa). Lazy-allocates at LED grid resolution via the
    /// EffectContext passed to `apply_effects`.
    led_group_effect_chains: Vec<EffectChain>,
    /// How many `led_group_bufs` / `led_group_effect_chains` were used last
    /// frame, for trim_excess_buffers.
    last_led_group_buf_used: usize,

    /// 1×1 opaque-black Rgba16Float texture used as a stand-in source when
    /// compositing **non-LED** layers into the LED stack. The non-L layer's
    /// own blend mode + opacity still apply, so an opaque Normal-blend layer
    /// substitutes black-on-black → covers what's below = matches the screen
    /// "blocked" semantic. Lazy-initialised on first frame any LED layer is
    /// active; cleared once on creation, then reused indefinitely.
    led_black_tex: Option<GpuTexture>,
}

/// Distinct owner_key for the LED master effect chain — must not collide with
/// owner_key 0 (main master) or any layer/clip hash.
const LED_MASTER_OWNER_KEY: i64 = i64::MIN + 1;

/// Returns true when blending an opaque-black source with this mode is a
/// mathematical no-op on the destination RGB.
///
/// Used by the LED path to skip dispatches for non-L layers that don't
/// actually block (these modes produce `out = base` when the foreground is
/// black). The non-skippable modes — Normal, Multiply, Overlay, Opaque,
/// Darken — *do* change the output and must run for correct screen-equivalent
/// blocking semantics.
///
/// Note: only safe to skip when the destination's alpha is already 1 (no
/// downstream blend reads a partial alpha channel). This holds for the
/// top-level led_main composite (cleared opaque) and blocker-group blends,
/// but **not** inside a per-group LED scratch (which is cleared transparent
/// and feeds group FX that may read alpha) — those keep running.
#[inline]
fn is_identity_for_black(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Additive
            | BlendMode::Screen
            | BlendMode::Stencil
            | BlendMode::Difference
            | BlendMode::Exclusion
            | BlendMode::Subtract
            | BlendMode::ColorDodge
            | BlendMode::Lighten
    )
}

/// Distinct owner_key for the LED group effect chain. Mirrors
/// `layer_id_owner_key` but mixes in a discriminator so temporal state on the
/// LED path doesn't collide with the same group's screen-path effect chain.
fn led_group_owner_key(layer_id: &manifold_core::LayerId) -> i64 {
    let mut hasher = DefaultHasher::new();
    layer_id.hash(&mut hasher);
    "led_group".hash(&mut hasher);
    (hasher.finish() | (1 << 63)) as i64
}


impl LayerCompositor {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        Self {
            main: PingPong::new(device, None, width, height, "Compositor"),
            layer_bufs: Vec::new(),
            blend: BlendResources::new(device, width, height),
            uniform_arena: UniformArena::new(device),
            effect_chains: Vec::new(),
            master_effect_chain: EffectChain::new(),
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
            group_bufs: Vec::new(),
            group_effect_chains: Vec::new(),
            last_group_buf_used: 0,
            group_child_indices: Vec::new(),
            group_child_positions: Vec::new(),
            led_main: None,
            led_tonemap: None,
            led_master_ec: None,
            led_group_bufs: Vec::new(),
            led_group_effect_chains: Vec::new(),
            last_led_group_buf_used: 0,
            led_black_tex: None,
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

    /// Ensure we have at least `count` LED group scratch buffers at the given
    /// LED grid resolution. Existing buffers are resized to match if needed.
    fn ensure_led_group_bufs(
        &mut self,
        count: usize,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        width: u32,
        height: u32,
    ) {
        while self.led_group_bufs.len() < count {
            let idx = self.led_group_bufs.len();
            self.led_group_bufs.push(PingPong::new(
                device,
                pool,
                width,
                height,
                &format!("LED Group Scratch {idx}"),
            ));
        }
        for buf in &mut self.led_group_bufs {
            if buf.width() != width || buf.height() != height {
                buf.resize(device, width, height);
            }
        }
    }

    /// Ensure we have at least `count` LED group effect chains available.
    /// Internal effect-chain buffers lazy-allocate at the LED grid resolution
    /// via the EffectContext passed to `apply_effects`.
    fn ensure_led_group_effect_chains(&mut self, count: usize) {
        while self.led_group_effect_chains.len() < count {
            self.led_group_effect_chains.push(EffectChain::new());
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

        let target_group_bufs = self.last_group_buf_used.saturating_add(HEADROOM);
        if self.group_bufs.len() > target_group_bufs {
            self.group_bufs.truncate(target_group_bufs);
        }
        if self.group_effect_chains.len() > target_group_bufs {
            self.group_effect_chains.truncate(target_group_bufs);
        }

        let target_led_group_bufs = self.last_led_group_buf_used.saturating_add(HEADROOM);
        if self.led_group_bufs.len() > target_led_group_bufs {
            self.led_group_bufs.truncate(target_led_group_bufs);
        }
        if self.led_group_effect_chains.len() > target_led_group_bufs {
            self.led_group_effect_chains.truncate(target_led_group_bufs);
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
    fn generate_layers(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame, any_solo: bool) {
        let clips = frame.clips;
        let width = self.main.width();
        let height = self.main.height();

        // Count active layers for pool sizing. Also track the maximum
        // layer_index in use this frame — effect chains are indexed by
        // layer_index (not by iteration order) so the chain bound to a
        // given layer stays the SAME `EffectChain` instance across
        // frames. Without that invariant, when the active-clip set
        // shifts (different clips firing each frame), `EffectChain` at
        // iteration-index 0 sees layer A's effects one frame and
        // layer B's the next — every frame becomes a topology
        // mismatch, the cached `ChainGraph` rebuilds, every
        // stateful primitive wipes its mip pyramid / feedback buffer
        // / pipeline cache, and the GPU does ~10ms of extra
        // first-evaluate work per frame.
        let mut multi_clip_layer_count = 0usize;
        let mut max_active_layer_idx: i32 = -1;
        {
            let mut ci = 0;
            while ci < clips.len() {
                let layer_idx = clips[ci].layer_index;
                let layer_desc = frame.find_layer(layer_idx);
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
                if layer_idx > max_active_layer_idx {
                    max_active_layer_idx = layer_idx;
                }
                if clip_count > 1 || has_layer_effects {
                    multi_clip_layer_count += 1;
                }
            }
        }

        // Ensure enough effect chains and scratch buffers.
        // `effect_chains[i]` is dedicated to layer `i` — size to
        // (max layer_idx + 1) so every active layer has a stable slot.
        // Inactive layers' slots stay empty (no `ChainGraph` cached,
        // no per-frame work).
        let chains_needed = (max_active_layer_idx + 1).max(0) as usize;
        self.ensure_effect_chains(chains_needed);
        if multi_clip_layer_count > 0 {
            self.ensure_layer_bufs(multi_clip_layer_count, gpu.device, gpu.pool);
        }

        self.layer_outputs_scratch.clear();
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
            let layer_desc = frame.find_layer(layer_idx);

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

            // Skip fully transparent layers — no GPU work needed
            if layer_opacity <= 0.0 {
                continue;
            }

            // Check if this layer has layer-level effects
            let has_layer_effects = layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

            // Acquire this layer's effect chain — indexed by
            // `layer_idx` so each chain stays bound to its layer
            // across frames. The `ensure_effect_chains` call above
            // sized the Vec to `max_active_layer_idx + 1`, so this
            // is always in bounds. Per-iteration aliasing is safe:
            // the `i` loop guarantees a single `layer_idx` value
            // per iteration body.
            let ec_idx = layer_idx as usize;
            let effect_chain = unsafe { &mut *effect_chains_ptr.add(ec_idx) };

            if group.len() == 1 && !has_layer_effects {
                // Single clip with NO layer effects — pass texture straight through
                let clip = &group[0];

                self.layer_outputs_scratch.push(LayerOutput {
                    texture: clip.texture,
                    blend_mode: layer_blend,
                    opacity: layer_opacity * clip.opacity,
                    layer_index: layer_idx,
                    blit_to_led: layer_desc.is_some_and(|ld| ld.blit_to_led),
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
                        owner_key: layer_desc.map_or(0, |ld| layer_id_owner_key(ld.layer_id)),
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
                    layer_index: layer_idx,
                    blit_to_led: layer_desc.is_some_and(|ld| ld.blit_to_led),
                });
            }
        }

        // Record actual usage for trim_excess_buffers. Effect chains
        // are sized to (max_active_layer_idx + 1), so that's the
        // high-water mark for this frame.
        self.last_layer_buf_used = layer_buf_idx;
        self.last_effect_chain_used = chains_needed;
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

    /// Blend layers into the LED composite buffer with screen-equivalent
    /// blocking semantics.
    ///
    /// **L (LED-flagged) layers** contribute their actual texture with Normal
    /// blend + opacity (so multiple L layers stack predictably).
    ///
    /// **Non-L layers** are composited too — but with their texture replaced
    /// by a 1×1 opaque-black stand-in. Their actual blend mode + opacity still
    /// apply, so an opaque Normal-blend non-L layer above an L layer covers
    /// the L on the LED frame the same way it covers it on screen. Without
    /// this, a non-L layer that visually blocks an L layer on screen would
    /// still leak through to the LEDs.
    ///
    /// **Groups with at least one L child** ("L groups") fold all their
    /// children (with the L/non-L substitution above) into one per-group LED
    /// scratch buffer, apply group effects at LED resolution, then blend the
    /// result into `led_main` with Normal + group opacity. Group FX always
    /// run on the LED path so group-level colouring (hue shifts, grades) is
    /// preserved. **Groups with no L children** ("blocker groups") skip the
    /// inner composite (children would all be black anyway) and contribute a
    /// single black blend with the group's actual blend mode + opacity into
    /// `led_main`.
    ///
    /// **Optimisation:** layers entirely below the bottom-most L contribute
    /// nothing to the final LED frame (they'd be overwritten). Iteration
    /// starts at the lowest LayerOutput that is L-flagged or part of an L
    /// group, skipping the rest.
    ///
    /// Runs **before** `fold_groups` so child layers inside groups route via
    /// their own `blit_to_led` flag, independent of the parent group's flag.
    /// Composite resolution is `frame.led_composite_size` — the native LED
    /// grid (e.g. 8×120), so master FX and group FX cost are negligible.
    fn blend_layers_to_led(
        &mut self,
        gpu: &mut GpuEncoder,
        layer_outputs: &[LayerOutput],
        frame: &CompositorFrame,
    ) {
        let any_led = layer_outputs.iter().any(|o| o.blit_to_led);
        if !any_led {
            // No LED routing this frame — release resources.
            self.led_main = None;
            self.last_led_group_buf_used = 0;
            return;
        }

        let (w, h) = (
            frame.led_composite_size.0.max(1),
            frame.led_composite_size.1.max(1),
        );
        let needs_new = self
            .led_main
            .as_ref()
            .is_none_or(|l| l.width() != w || l.height() != h);
        if needs_new {
            self.led_main = Some(PingPong::new(gpu.device, gpu.pool, w, h, "LED Composite"));
        }

        // Lazy-create the 1×1 opaque-black stand-in texture used for non-L
        // layers' blocking blends. Cleared on the first frame; never modified
        // afterwards (no per-frame work).
        let black_tex_freshly_created = self.led_black_tex.is_none();
        if black_tex_freshly_created {
            self.led_black_tex = Some(gpu.device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                format: GpuTextureFormat::Rgba16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET_FULL,
                label: "LED Black 1x1",
            }));
        }
        // Raw pointers for disjoint borrows. Safety: this function is the sole
        // writer to led_main / led_group_bufs / led_black_tex during its
        // scope; pointers are valid until the function returns.
        let led_main_ptr = self.led_main.as_mut().unwrap() as *mut PingPong;
        let led_main = unsafe { &mut *led_main_ptr };
        led_main.clear_source(gpu, true);

        let black_tex_ref: &GpuTexture = self.led_black_tex.as_ref().unwrap();
        let black_tex_ptr: *const GpuTexture = black_tex_ref;
        if black_tex_freshly_created {
            // Initialise once to opaque black.
            gpu.clear_texture(black_tex_ref, 0.0, 0.0, 0.0, 1.0);
        }

        // Resolve each LayerOutput's parent group_id once (avoids repeated
        // O(N) lookups in the inner loop).
        let parent_ids: Vec<Option<&LayerId>> = layer_outputs
            .iter()
            .map(|o| {
                frame
                    .find_layer(o.layer_index)
                    .and_then(|ld| ld.parent_layer_id)
            })
            .collect();

        // Determine which group ids contain at least one L child — these are
        // "L groups" that need a per-group fold + group FX. Other groups are
        // "blocker groups" handled inline as a single black blend.
        let mut l_group_ids: Vec<&LayerId> = Vec::new();
        for (idx, output) in layer_outputs.iter().enumerate() {
            if output.blit_to_led
                && let Some(pid) = parent_ids[idx]
                && !l_group_ids.contains(&pid)
            {
                l_group_ids.push(pid);
            }
        }
        let is_l_group = |pid: &LayerId| l_group_ids.iter().any(|id| **id == *pid);

        // Find the bottom-most LayerOutput that's L or part of an L group.
        // Anything before this position can be skipped (would be overwritten).
        let start_idx = layer_outputs
            .iter()
            .enumerate()
            .position(|(idx, output)| {
                output.blit_to_led || parent_ids[idx].is_some_and(&is_l_group)
            })
            .unwrap_or(layer_outputs.len());

        let mut processed = vec![false; layer_outputs.len()];
        let mut led_group_idx = 0usize;

        for i in start_idx..layer_outputs.len() {
            if processed[i] {
                continue;
            }

            let parent_id = parent_ids[i];

            if let Some(pid) = parent_id {
                // Group child — handle the whole group once on first encounter.
                let group_desc = frame.layers.iter().find(|l| l.layer_id == pid);

                if is_l_group(pid) {
                    // L group: fold all children (L with own texture + Normal,
                    // non-L with black + actual blend mode), apply group FX,
                    // blend into led_main with Normal + group opacity.
                    if let Some(group) = group_desc {
                        self.ensure_led_group_bufs(led_group_idx + 1, gpu.device, gpu.pool, w, h);
                        self.ensure_led_group_effect_chains(led_group_idx + 1);
                        let group_buf_ptr =
                            &mut self.led_group_bufs[led_group_idx] as *mut PingPong;
                        let group_ec_ptr =
                            &mut self.led_group_effect_chains[led_group_idx] as *mut EffectChain;
                        led_group_idx += 1;
                        let group_buf = unsafe { &mut *group_buf_ptr };
                        let group_ec = unsafe { &mut *group_ec_ptr };

                        // Transparent initial state matches screen-path
                        // fold_groups so partial-opacity within group works.
                        group_buf.clear_source(gpu, false);

                        // Composite EVERY child of this group (L and non-L)
                        // in iteration order (bottom→top).
                        for j in i..layer_outputs.len() {
                            if processed[j] {
                                continue;
                            }
                            if parent_ids[j] != Some(pid) {
                                continue;
                            }
                            let (blend_mode, src_tex) = if layer_outputs[j].blit_to_led {
                                (BlendMode::Normal, layer_outputs[j].texture())
                            } else {
                                (
                                    layer_outputs[j].blend_mode,
                                    // Safety: ptr targets the persistent
                                    // led_black_tex created above.
                                    unsafe { &*black_tex_ptr },
                                )
                            };
                            let child_uniforms = BlendUniforms {
                                blend_mode: blend_mode as u32,
                                opacity: layer_outputs[j].opacity,
                                _pad0: 0,
                                _pad1: 0,
                            };
                            self.blend.blend_pass(
                                gpu,
                                &mut self.uniform_arena,
                                group_buf.source_texture(),
                                src_tex,
                                group_buf.target_texture(),
                                &child_uniforms,
                            );
                            group_buf.swap();
                            processed[j] = true;
                        }

                        // Apply group effects (if any) at LED resolution.
                        let group_source: *const GpuTexture = if has_enabled_effects(group.effects) {
                            let ctx = EffectContext {
                                time: frame.time,
                                beat: frame.beat,
                                dt: frame.dt,
                                width: w,
                                height: h,
                                output_width: frame.output_width,
                                output_height: frame.output_height,
                                owner_key: led_group_owner_key(group.layer_id),
                                is_clip_level: false,
                                edge_stretch_width: 0.5625,
                                frame_count: frame.frame_count as i64,
                            };
                            match Self::apply_effects(
                                group_ec,
                                &mut self.effect_registry,
                                &self.wet_dry_lerp,
                                gpu,
                                group_buf.source_texture(),
                                group.effects,
                                group.effect_groups,
                                &ctx,
                            ) {
                                Some(t) => t,
                                None => group_buf.source_texture() as *const _,
                            }
                        } else {
                            group_buf.source_texture() as *const _
                        };

                        // Blend group result into led_main with Normal +
                        // group opacity (Normal everywhere on LED path).
                        let final_uniforms = BlendUniforms {
                            blend_mode: BlendMode::Normal as u32,
                            opacity: group.opacity,
                            _pad0: 0,
                            _pad1: 0,
                        };
                        self.blend.blend_pass(
                            gpu,
                            &mut self.uniform_arena,
                            led_main.source_texture(),
                            unsafe { &*group_source },
                            led_main.target_texture(),
                            &final_uniforms,
                        );
                        led_main.swap();
                    }
                } else {
                    // Blocker group (no L children): a single BLACK blend
                    // with the group's actual blend_mode + opacity. Skip the
                    // dispatch entirely if the blend mode is identity-for-
                    // black (Add / Screen / etc.) — the group can't change
                    // led_main with a black source. Then mark all the
                    // group's children processed regardless.
                    if let Some(group) = group_desc
                        && !is_identity_for_black(group.blend_mode)
                    {
                        let uniforms = BlendUniforms {
                            blend_mode: group.blend_mode as u32,
                            opacity: group.opacity,
                            _pad0: 0,
                            _pad1: 0,
                        };
                        self.blend.blend_pass(
                            gpu,
                            &mut self.uniform_arena,
                            led_main.source_texture(),
                            unsafe { &*black_tex_ptr },
                            led_main.target_texture(),
                            &uniforms,
                        );
                        led_main.swap();
                    }
                    for j in i..layer_outputs.len() {
                        if parent_ids[j] == Some(pid) {
                            processed[j] = true;
                        }
                    }
                }
            } else {
                // Top-level layer.
                let is_l = layer_outputs[i].blit_to_led;
                // Skip non-L blends with identity-for-black blend modes —
                // they can't change led_main with a black source.
                if !is_l && is_identity_for_black(layer_outputs[i].blend_mode) {
                    processed[i] = true;
                    continue;
                }
                let (blend_mode, src_tex) = if is_l {
                    (BlendMode::Normal, layer_outputs[i].texture())
                } else {
                    (
                        layer_outputs[i].blend_mode,
                        unsafe { &*black_tex_ptr },
                    )
                };
                let uniforms = BlendUniforms {
                    blend_mode: blend_mode as u32,
                    opacity: layer_outputs[i].opacity,
                    _pad0: 0,
                    _pad1: 0,
                };
                self.blend.blend_pass(
                    gpu,
                    &mut self.uniform_arena,
                    led_main.source_texture(),
                    src_tex,
                    led_main.target_texture(),
                    &uniforms,
                );
                led_main.swap();
                processed[i] = true;
            }
        }

        self.last_led_group_buf_used = led_group_idx;
    }

    /// Fold group children into single LayerOutputs.
    ///
    /// For each group layer that has children in layer_outputs_scratch:
    /// 1. Composite child outputs into a group scratch buffer
    /// 2. Apply group-level effects
    /// 3. Replace child entries with a single output carrying the group's blend/opacity
    ///
    /// No-op when no groups exist (single boolean check).
    fn fold_groups(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) {
        // Early exit: no groups → nothing to fold
        if !frame.layers.iter().any(|l| l.is_group) {
            self.last_group_buf_used = 0;
            return;
        }

        let width = self.main.width();
        let height = self.main.height();
        let mut group_buf_idx = 0usize;

        // Process each group. Groups are processed in the order they appear in
        // frame.layers (which matches timeline order). Since outputs are sorted
        // descending by layer_index, children of a group are contiguous.
        for group_desc in frame.layers.iter().filter(|l| l.is_group) {
            // Find child layer_indices for this group (reuse scratch buffer).
            self.group_child_indices.clear();
            for l in frame.layers {
                if l.parent_layer_id.as_ref() == Some(&group_desc.layer_id) {
                    self.group_child_indices.push(l.layer_index);
                }
            }

            if self.group_child_indices.is_empty() {
                continue;
            }

            // Find which outputs belong to children of this group.
            self.group_child_positions.clear();
            for (i, o) in self.layer_outputs_scratch.iter().enumerate() {
                if self.group_child_indices.contains(&o.layer_index) {
                    self.group_child_positions.push(i);
                }
            }

            if self.group_child_positions.is_empty() {
                continue;
            }

            // Each group gets its own PingPong — raw pointers from earlier
            // groups must remain valid until blend_layers.
            let gb_idx = group_buf_idx;
            group_buf_idx += 1;
            while self.group_bufs.len() <= gb_idx {
                let idx = self.group_bufs.len();
                self.group_bufs.push(PingPong::new(
                    gpu.device,
                    gpu.pool,
                    width,
                    height,
                    &format!("Group Scratch {idx}"),
                ));
            }
            let group_buf = &mut self.group_bufs[gb_idx];

            // Clear to transparent
            group_buf.clear_source(gpu, false);

            // Blend children into group buffer (using each child's own blend/opacity)
            for &pos in &self.group_child_positions {
                let output = &self.layer_outputs_scratch[pos];
                let uniforms = BlendUniforms {
                    blend_mode: output.blend_mode as u32,
                    opacity: output.opacity,
                    _pad0: 0,
                    _pad1: 0,
                };
                self.blend.blend_pass(
                    gpu,
                    &mut self.uniform_arena,
                    group_buf.source_texture(),
                    output.texture(),
                    group_buf.target_texture(),
                    &uniforms,
                );
                group_buf.swap();
            }

            // Apply group-level effects (if any)
            let group_texture: *const GpuTexture =
                if has_enabled_effects(group_desc.effects) {
                    // Each group gets its own effect chain too
                    while self.group_effect_chains.len() <= gb_idx {
                        self.group_effect_chains.push(EffectChain::new());
                    }
                    let effect_chain = &mut self.group_effect_chains[gb_idx];
                    let ctx = EffectContext {
                        time: frame.time,
                        beat: frame.beat,
                        dt: frame.dt,
                        width,
                        height,
                        output_width: frame.output_width,
                        output_height: frame.output_height,
                        owner_key: group_id_owner_key(group_desc.layer_id),
                        is_clip_level: false,
                        edge_stretch_width: 0.5625,
                        frame_count: frame.frame_count as i64,
                    };
                    let group_buf = &self.group_bufs[gb_idx];
                    let result = Self::apply_effects(
                        effect_chain,
                        &mut self.effect_registry,
                        &self.wet_dry_lerp,
                        gpu,
                        group_buf.source_texture(),
                        group_desc.effects,
                        group_desc.effect_groups,
                        &ctx,
                    );
                    result.map_or(group_buf.source_texture() as *const _, |t| {
                        t as *const _
                    })
                } else {
                    self.group_bufs[gb_idx].source_texture()
                };

            // Replace child outputs with a single group output.
            // Insert group output at the first child's position, remove the rest.
            let first_pos = self.group_child_positions[0];
            self.layer_outputs_scratch[first_pos] = LayerOutput {
                texture: group_texture,
                blend_mode: group_desc.blend_mode,
                opacity: group_desc.opacity,
                layer_index: group_desc.layer_index,
                blit_to_led: group_desc.blit_to_led,
            };

            // Remove remaining child entries (iterate in reverse to preserve indices)
            for &pos in self.group_child_positions[1..].iter().rev() {
                self.layer_outputs_scratch.remove(pos);
            }
        }

        self.last_group_buf_used = group_buf_idx;
    }

    /// Serial composite path: single encoder for all work.
    /// Used when only 1 active layer (no parallel benefit).
    fn composite_serial(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame, any_solo: bool) {
        self.uniform_arena.reset();
        self.generate_layers(gpu, frame, any_solo);
        // Route LED-flagged layers BEFORE folding groups so child layers inside
        // a group route via their own blit_to_led flag (the group's flag controls
        // only the screen-output blend, not LED routing).
        // Safety: same lifetime guarantees as the blend_layers call below.
        let pre_fold_outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let pre_fold_outputs_len = self.layer_outputs_scratch.len();
        let pre_fold_outputs = unsafe {
            std::slice::from_raw_parts(pre_fold_outputs_ptr, pre_fold_outputs_len)
        };
        self.blend_layers_to_led(gpu, pre_fold_outputs, frame);

        self.fold_groups(gpu, frame);
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
    fn composite_parallel(
        &mut self,
        compositor_gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
        any_solo: bool,
    ) {
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

        // Pre-scan: count multi-clip layers for pool sizing, and
        // track the max layer_index in use this frame. Effect chains
        // are indexed by `layer_idx` (not by iteration order) so
        // each chain stays bound to its layer across frames — see
        // the matching comment in `generate_layers` for the
        // rebuild-thrash bug this prevents.
        let mut multi_clip_layer_count = 0usize;
        let mut max_active_layer_idx: i32 = -1;
        {
            let mut ci = 0;
            while ci < clips.len() {
                let layer_idx = clips[ci].layer_index;
                let layer_desc = frame.find_layer(layer_idx);
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
                if layer_idx > max_active_layer_idx {
                    max_active_layer_idx = layer_idx;
                }
                if clip_count > 1 || has_layer_effects {
                    multi_clip_layer_count += 1;
                }
            }
        }

        let chains_needed = (max_active_layer_idx + 1).max(0) as usize;
        self.ensure_effect_chains(chains_needed);
        if multi_clip_layer_count > 0 {
            self.ensure_layer_bufs(multi_clip_layer_count, device, pool);
        }

        let effect_chains_ptr = self.effect_chains.as_mut_ptr();
        let layer_bufs_ptr = self.layer_bufs.as_mut_ptr();
        let base_signal = self.async_signal_base;

        self.layer_outputs_scratch.clear();
        let mut layer_buf_idx = 0usize;
        let mut layer_signal_idx = 0u64;

        // Process each layer on its own command buffer.
        let mut i = 0;
        while i < clips.len() {
            let layer_idx = clips[i].layer_index;
            let layer_desc = frame.find_layer(layer_idx);

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

            // Skip fully transparent layers — no GPU work needed
            if layer_opacity <= 0.0 {
                continue;
            }

            let has_layer_effects = layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

            // Effect chain is indexed by `layer_idx` so it stays
            // bound to this layer across frames — see the matching
            // block in `generate_layers` for the rebuild-thrash
            // bug this prevents. `ensure_effect_chains` above sized
            // the Vec to `max_active_layer_idx + 1`.
            let ec_idx = layer_idx as usize;
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
                        layer_index: layer_idx,
                        blit_to_led: layer_desc.is_some_and(|ld| ld.blit_to_led),
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
                            owner_key: layer_desc.map_or(0, |ld| layer_id_owner_key(ld.layer_id)),
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
                        layer_index: layer_idx,
                        blit_to_led: layer_desc.is_some_and(|ld| ld.blit_to_led),
                    });
                }
            } // gpu wrapper drops here, releasing borrow on layer_enc

            // Signal completion for this layer and commit
            layer_signal_idx += 1;
            let signal_value = base_signal + layer_signal_idx;
            layer_enc.signal_event_value(unsafe { &*async_event }, signal_value);
            layer_enc.commit();
        }

        // Record actual usage for trim_excess_buffers. Effect chains
        // are sized to (max_active_layer_idx + 1), so that's the
        // high-water mark for this frame.
        self.last_layer_buf_used = layer_buf_idx;
        self.last_effect_chain_used = chains_needed;

        // Update base for next frame
        self.async_signal_base = base_signal + layer_signal_idx;

        // Compositor command buffer waits for all layer completions
        let final_signal = base_signal + layer_signal_idx;
        if layer_signal_idx > 0 {
            compositor_gpu
                .native_enc
                .wait_event(unsafe { &*async_event }, final_signal);
        }

        // Route LED-flagged layers BEFORE folding groups so child layers inside
        // a group route via their own blit_to_led flag.
        // Safety: same as below — outputs are valid for frame duration.
        let pre_fold_outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let pre_fold_outputs_len = self.layer_outputs_scratch.len();
        let pre_fold_outputs = unsafe {
            std::slice::from_raw_parts(pre_fold_outputs_ptr, pre_fold_outputs_len)
        };
        self.blend_layers_to_led(compositor_gpu, pre_fold_outputs, frame);

        // Fold group children into single outputs before blending.
        self.fold_groups(compositor_gpu, frame);

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
            // Release LED composite resources (nothing to route).
            self.led_main = None;
            self.led_tonemap = None;
            self.led_master_ec = None;
            self.last_led_group_buf_used = 0;
            return &self.tonemap.output.texture;
        }

        // Choose serial vs parallel composite path.
        // Parallel path creates per-layer command buffers for GPU-concurrent
        // generation. Only activated with 2+ active layers (no overhead for
        // single-layer frames).
        let any_solo = frame.layers.iter().any(|l| l.is_solo);
        #[cfg(target_os = "macos")]
        {
            let active_layers = count_active_layers(frame, any_solo);
            if active_layers >= 2 {
                self.composite_parallel(gpu, frame, any_solo);
            } else {
                self.composite_serial(gpu, frame, any_solo);
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.composite_serial(gpu, frame, any_solo);

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

            // Master effects use a dedicated `EffectChain` instance,
            // separate from the per-layer chains. Layer chains are
            // now indexed by `layer_idx`, so reusing
            // `effect_chains[0]` here would collide with layer 0's
            // chain — every frame would force a `ChainGraph` rebuild
            // alternating between layer 0's effects and the master
            // effects, wiping primitive state.
            let master_ec = &mut self.master_effect_chain;

            // Feed tonemap output directly into the effect chain — the first
            // effect reads from tonemap.output without copying.
            if let Some(processed) = Self::apply_effects(
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
                // Use the texture `apply_effects` returned directly — under the
                // ChainGraph fast path, the result lives in the chain graph's
                // backend (not in `master_ec.ping`/`pong`, which stay None for
                // graph-dispatched chains). `source_texture_pub()` would
                // unwrap a None ping in that case.
                gpu.copy_texture_to_texture(
                    processed,
                    &self.tonemap.output.texture,
                    width,
                    height,
                );
            }
        }

        // ── LED composite: tonemap + master FX (gated by led_exit_index) ──
        // The LED composite is built at native LED grid resolution, so master
        // FX cost is negligible. `led_exit_index` controls whether to run them:
        //   * `0` (pre-tonemap tap) — skip both tonemap and master FX. The raw
        //     blended composite goes straight to the LED edge-extend pass. This
        //     is the user's escape hatch for FX that don't translate to LEDs.
        //   * `-1` (default, post-effects) — apply tonemap + master FX so the
        //     LED color treatment matches the screen.
        if let Some(ref led_main) = self.led_main
            && frame.led_exit_index == -1
        {
            let (width, height) = frame.led_composite_size;
            let width = width.max(1);
            let height = height.max(1);

            // Lazy-init / resize LED tonemap pipeline.
            let needs_new_tonemap = self.led_tonemap.as_ref().is_none_or(|t| {
                t.output.width != width || t.output.height != height
            });
            if needs_new_tonemap {
                self.led_tonemap = Some(TonemapPipeline::new(gpu.device, width, height));
            }

            // Tonemap the LED composite (same settings as main).
            let led_source_tex_ptr: *const GpuTexture = led_main.source_texture();
            // Safety: led_source_tex_ptr points to led_main.ping/pong which are not
            // reallocated between here and the apply() call below.
            self.led_tonemap
                .as_ref()
                .unwrap()
                .apply(gpu, unsafe { &*led_source_tex_ptr }, &frame.tonemap);

            // Apply master FX to the LED tonemap output (distinct owner_key so
            // temporal state doesn't bleed between main and LED master chains).
            if has_enabled_effects(frame.master_effects) {
                let led_ec = self.led_master_ec.get_or_insert_with(EffectChain::new);

                let ctx = EffectContext {
                    time: frame.time,
                    beat: frame.beat,
                    dt: frame.dt,
                    width,
                    height,
                    output_width: frame.output_width,
                    output_height: frame.output_height,
                    owner_key: LED_MASTER_OWNER_KEY,
                    is_clip_level: false,
                    edge_stretch_width: 0.5625,
                    frame_count: frame.frame_count as i64,
                };

                let led_tm_tex_ptr: *const GpuTexture =
                    &self.led_tonemap.as_ref().unwrap().output.texture;
                // Safety: led_tm_tex_ptr points to led_tonemap.output.texture which
                // is not reallocated during apply_effects.
                if let Some(processed) = Self::apply_effects(
                    led_ec,
                    &mut self.effect_registry,
                    &self.wet_dry_lerp,
                    gpu,
                    unsafe { &*led_tm_tex_ptr },
                    frame.master_effects,
                    frame.master_effect_groups,
                    &ctx,
                ) {
                    // Use the texture `apply_effects` returned directly — see
                    // the master_ec path above for why `source_texture_pub`
                    // doesn't work post-ChainGraph cutover.
                    gpu.copy_texture_to_texture(
                        processed,
                        unsafe { &*led_tm_tex_ptr },
                        width,
                        height,
                    );
                }
            }
        } else {
            // No LED layers active, or exit-path bypasses tonemap+FX — release
            // tonemap/effect-chain resources. led_main itself is kept (it is
            // the LED source texture when exit_index == 0) and freed only when
            // no layer is flagged at all (handled in blend_layers_to_led).
            self.led_tonemap = None;
            self.led_master_ec = None;
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
        self.master_effect_chain.resize(device, width, height);
        self.effect_registry.resize_all(device, width, height);
        self.tonemap.resize(device, width, height);
        for gb in &mut self.group_bufs {
            gb.resize(device, width, height);
        }
        for ec in &mut self.group_effect_chains {
            ec.resize(device, width, height);
        }
        // LED tap will be recreated at new size on next frame if needed.
        // The per-layer LED composite size comes from `frame.led_composite_size`
        // (the native LED grid), independent of compositor resolution — its
        // buffers reallocate lazily in blend_layers_to_led / render() if the
        // frame's LED size differs.
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

    fn led_composite_texture(&self) -> Option<&GpuTexture> {
        // Present only when at least one layer was flagged `blit_to_led` this
        // frame. Returns the tonemapped + master-FX-processed result when the
        // LED exit path applied them (exit_index == -1), otherwise the raw
        // blended composite (exit_index == 0).
        if let Some(tm) = self.led_tonemap.as_ref() {
            Some(&tm.output.texture)
        } else {
            self.led_main.as_ref().map(|l| l.source_texture())
        }
    }

    fn graph_snapshot_for(
        &self,
        type_id: &manifold_core::EffectTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        self.effect_registry.graph_snapshot_for(type_id)
    }
}
