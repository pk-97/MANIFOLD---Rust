use crate::chain_dispatch::{clear_chain_state, dispatch_chain};
use crate::compositor::{CompositeLayerDescriptor, Compositor, CompositorFrame};
use crate::effect::PostProcessEffect;
use crate::preset_runtime::PresetRuntime;
use crate::gpu_encoder::GpuEncoder;
use crate::preset_context::PresetContext;
use crate::render_target::RenderTarget;
use crate::tonemap::TonemapPipeline;
use crate::uniform_arena::UniformArena;
use ahash::AHashMap;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::{BlendMode, EffectId, PresetTypeId, LayerId, NodeId};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
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
    pub effects: &'a [PresetInstance],
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
            ..Default::default()
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
fn has_enabled_effects(effects: &[PresetInstance]) -> bool {
    for fx in effects {
        if fx.enabled
            && *fx.effect_type() != PresetTypeId::UNKNOWN
            && fx.params.iter().next().map(|p| p.value).unwrap_or(0.0) > 0.0
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

/// §24 5c with-effects thumbnails: a single-clip layer's post-effect output,
/// keyed by clip id. Unlike `LayerOutput`, this is consumed LATER in the frame
/// by the clip-thumbnail snapshot, by which time the layer/effect render target
/// it came from may have been recycled. So it owns a `GpuTexture` clone (one
/// atomic retain on the underlying Metal texture — no GPU allocation) to keep
/// that texture alive until the snapshot reads it. A raw pointer here was the
/// cause of a hard AGX crash when the target was freed before the blit.
pub(crate) struct ClipPostFx {
    clip_id: String,
    texture: GpuTexture,
}

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
    /// Per-layer scratch buffers (lazy, transparent black init),
    /// keyed by `LayerId`. One per active multi-clip-or-effects
    /// layer; reused across frames AND across layer reorders.
    /// PingPongs are cleared at the start of each layer's use, so
    /// no state can contaminate across layers within a frame —
    /// the keying is purely for consistency with the chain pools.
    layer_bufs: AHashMap<LayerId, PingPong>,
    /// Per-layer-buf last-used frame counter for time-based pruning.
    layer_buf_last_used_frame: AHashMap<LayerId, u64>,
    /// GPU resources for blend operations (pipeline, sampler).
    blend: BlendResources,
    /// Per-frame uniform sub-allocator — batches all blend uniform writes into
    /// a single buffer. On native path, arena buffer is not read (uses inline
    /// set_bytes), but offset tracking is preserved.
    uniform_arena: UniformArena,
    /// Per-layer effect chain processors, keyed by `LayerId`. Each
    /// chain stays bound to its layer for the lifetime of the project,
    /// so its cached `PresetRuntime` (primitive instances + state)
    /// survives across frames even as clips fire/end AND across layer
    /// reorders (LayerId is stable; `layer_index` is not). Inactive
    /// layers' chains are dropped after `CHAIN_GRACE_FRAMES` of disuse.
    /// Type-level invariant: the key is `LayerId`, not `usize`, so
    /// iteration-counter indexing won't compile.
    effect_chains: AHashMap<LayerId, Option<PresetRuntime>>,
    /// Per-chain last-used frame counter. Parallel to `effect_chains`;
    /// updated each frame the chain is touched. Trimmed once stale.
    chain_last_used_frame: AHashMap<LayerId, u64>,
    /// Monotonic frame counter for chain liveness tracking. Wraps at
    /// u64 (~10⁹ years at 60 fps — i.e., never).
    frame_counter: u64,
    /// Scratch buffer reused each frame to collect active layer IDs
    /// during pre-scan. Stored on `self` to avoid per-frame allocation.
    active_layer_ids_scratch: Vec<LayerId>,
    /// Subset of `active_layer_ids_scratch` whose layers also need a
    /// layer scratch buffer (multi-clip or has-layer-effects).
    /// Pre-scanned so `ensure_layer_buf` can run before the main loop
    /// (no insertions mid-iteration → safe `get_mut`).
    active_layer_buf_ids_scratch: Vec<LayerId>,
    /// Dedicated effect chain for the post-blend master FX pass.
    /// Kept SEPARATE from `effect_chains` because the master pass
    /// has no natural `LayerId` to key by — it operates on the
    /// composited scene, not a layer. A dedicated field makes the
    /// distinction structural (different type, different field)
    /// so master FX and layer FX cannot share a chain. Costs ~56
    /// bytes of idle struct space when unused and zero CPU when
    /// no master effects are present.
    master_effect_chain: Option<PresetRuntime>,
    /// Plugin warmup processors — held for the process lifetime so
    /// background FFI workers (BlobDetector, DepthEstimator,
    /// WireframeDepth) stay alive. The compositor forwards `resize`
    /// and `flush_background_work` through them; chain dispatch goes
    /// through the primitive registry, not these handles. See
    /// [`crate::plugin_prewarm`].
    plugin_warmups: Vec<Box<dyn PostProcessEffect>>,
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
    /// §24 5c with-effects thumbnails: `clip_id → that layer's post-effect output
    /// texture`, populated only for SINGLE-clip layers (where the layer output IS
    /// that clip's full look — generator/video + layer effects). Multi-clip layers
    /// can't isolate one clip, so they're absent and the thumbnail uses the raw
    /// clip texture. Raw pointers valid for the frame (like `LayerOutput`); read
    /// only on the content thread, same frame. Cleared each `generate_layers`.
    clip_post_fx_scratch: Vec<ClipPostFx>,
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
    /// Per-group scratch buffers (lazy, transparent black init),
    /// keyed by the group container's `LayerId`. One per active
    /// group — each group needs its own buffer because LayerOutput
    /// raw pointers must remain valid until blend_layers.
    group_bufs: AHashMap<LayerId, PingPong>,
    /// Per-group-buf last-used frame counter for time-based pruning.
    group_buf_last_used_frame: AHashMap<LayerId, u64>,
    /// Per-group effect chains, keyed by the group container's
    /// `LayerId`. Same structural invariant as `effect_chains`:
    /// the key is stable, iteration-counter indexing won't compile.
    group_effect_chains: AHashMap<LayerId, Option<PresetRuntime>>,
    /// Per-group-chain last-used frame counter for time-based pruning.
    group_chain_last_used_frame: AHashMap<LayerId, u64>,
    /// Pre-allocated scratch for child layer indices during group folding.
    group_child_indices: Vec<i32>,
    /// Pre-allocated scratch for child output positions during group folding.
    group_child_positions: Vec<usize>,

    // ── LED per-layer routing ──
    /// Accumulation buffer for layers flagged `blit_to_led`. Lazily allocated on
    /// the first frame any LED layer is active; persistent across frames.
    /// The LED path runs raw HDR end-to-end — no dedicated tonemap stage. The
    /// screen tonemap is wrong for LEDs (its peak target is the TV's display
    /// nits, far below the LEDs' headroom) and its per-channel soft-clip
    /// washed colored bright peaks toward white. The LED slicer applies
    /// `led_gain` and a chroma-preserving clip in linear space before the
    /// 8-bit DMX clamp instead.
    led_main: Option<PingPong>,
    /// Dedicated effect chain for LED master FX. Stored as a standalone field
    /// (not in `effect_chains` Vec) so the shared resize path doesn't force it
    /// to full resolution — the LED chain auto-allocates at half-res via
    /// `ensure_buffers` driven by the LED PresetContext.
    /// Uses owner_key `LED_MASTER_OWNER_KEY` to keep temporal state separate
    /// from the main master chain.
    led_master_ec: Option<Option<PresetRuntime>>,

    /// Per-group LED scratch buffers at LED grid resolution, keyed
    /// by the group container's `LayerId`. One per group whose
    /// LED-flagged children need to flow through that group's
    /// effects on the LED path. Reused across frames; reallocated
    /// only on LED-grid dimension changes.
    led_group_bufs: AHashMap<LayerId, PingPong>,
    /// Per-LED-group-buf last-used frame counter for time-based pruning.
    led_group_buf_last_used_frame: AHashMap<LayerId, u64>,
    /// Per-group LED effect chains, keyed by the group container's
    /// `LayerId`. Distinct field from `group_effect_chains` so
    /// temporal state on the LED path doesn't bleed into the screen
    /// path (and vice versa) — physically impossible because they
    /// are different fields, even though both use `LayerId` keys.
    /// Lazy-allocates at LED grid resolution via the `PresetContext`
    /// passed to `apply_effects`.
    led_group_effect_chains: AHashMap<LayerId, Option<PresetRuntime>>,
    /// Per-LED-group-chain last-used frame counter for time-based pruning.
    led_group_chain_last_used_frame: AHashMap<LayerId, u64>,

    /// 1×1 opaque-black Rgba16Float texture used as a stand-in source when
    /// compositing **non-LED** layers into the LED stack. The non-L layer's
    /// own blend mode + opacity still apply, so an opaque Normal-blend layer
    /// substitutes black-on-black → covers what's below = matches the screen
    /// "blocked" semantic. Lazy-initialised on first frame any LED layer is
    /// active; cleared once on creation, then reused indefinitely.
    led_black_tex: Option<GpuTexture>,

    /// Authoring-time node-output preview request: `(watched effect, optional
    /// selected node)`. Applied to every chain before `render` so the chain
    /// holding the watched effect preserves the selected node's output for the
    /// editor to sample; all other chains clear. `None` = no preview active.
    /// Set via [`Self::set_preview_request`]; read back via
    /// [`Self::preview_texture`].
    preview_request: Option<(EffectId, Option<NodeId>)>,

    /// Dump request applied to the watched effect's chain before the next
    /// `render` — either the Cmd+D whole-graph disk dump or the editor's
    /// visible-only thumbnail atlas. The content layer reads
    /// [`Self::dump_textures`] after that frame. `None` = no dump pending.
    dump_request: Option<crate::compositor::DumpRequest>,

    /// Per-step GPU/CPU attribution profiling on/off for every chain this
    /// compositor owns (PERF_BUDGET_GATE_DESIGN P2 / D6). Fanned out to each
    /// chain's executor at dispatch time (see `fx_scope`/`led_scope`
    /// helpers) — `false` costs one `bool` set per dispatch, zero GPU/CPU
    /// timing. Set via [`Self::set_profiling`].
    profiling_enabled: bool,
    /// Force the serial composite path (D6 correction: profiled mode needs
    /// one shared compositor command buffer to attach the dispatch sampler
    /// to). Set via [`Self::set_force_serial`].
    force_serial: bool,
}

/// This chain's profiled-tag scope for a screen/LED per-layer effect chain:
/// `fx:{layer_id}`.
fn fx_scope(layer_id: &LayerId) -> String {
    format!("fx:{layer_id}")
}

/// This chain's profiled-tag scope for a per-group-on-the-LED-path effect
/// chain: `led:{group_id}`.
fn led_scope(group_id: &LayerId) -> String {
    format!("led:{group_id}")
}

/// Distinct owner_key for the LED master effect chain — must not collide with
/// owner_key 0 (main master) or any layer/clip hash.
const LED_MASTER_OWNER_KEY: i64 = i64::MIN + 1;

/// How many render() calls a per-layer effect chain may stay unused before
/// it's dropped as a memory-hygiene safety net. Acts ALONGSIDE the
/// event-based eviction in `trim_excess_buffers` (which drops a chain
/// the moment its `LayerId` disappears from `frame.layers`). The timer
/// is the catch for the "this layer hasn't been used in ages and the
/// operator has clearly moved on" case in multi-hour live shows — frees
/// memory for sections that won't be revisited without waiting for the
/// project to be edited.
///
/// 18000 = 5 minutes at 60 fps. Frame-count, not wall time, so a 30-fps
/// project effectively gets 10 min, a 120-fps project 2.5 min. Comfortable
/// margin around typical mid-song mutes / song-to-song transitions
/// (sub-minute) while still freeing memory inside a long show.
const CHAIN_GRACE_FRAMES: u64 = 18000;

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
            layer_bufs: AHashMap::default(),
            layer_buf_last_used_frame: AHashMap::default(),
            blend: BlendResources::new(device, width, height),
            uniform_arena: UniformArena::new(device),
            effect_chains: AHashMap::default(),
            chain_last_used_frame: AHashMap::default(),
            frame_counter: 0,
            active_layer_ids_scratch: Vec::new(),
            active_layer_buf_ids_scratch: Vec::new(),
            master_effect_chain: None,
            plugin_warmups: crate::plugin_prewarm::prewarm_all(device),
            tonemap: TonemapPipeline::new(device, width, height),
            led_tap: None,
            layer_outputs_scratch: Vec::new(),
            clip_post_fx_scratch: Vec::new(),
            #[cfg(target_os = "macos")]
            async_event: None,
            #[cfg(target_os = "macos")]
            async_signal_base: 0,
            group_bufs: AHashMap::default(),
            group_buf_last_used_frame: AHashMap::default(),
            group_effect_chains: AHashMap::default(),
            group_chain_last_used_frame: AHashMap::default(),
            group_child_indices: Vec::new(),
            group_child_positions: Vec::new(),
            led_main: None,
            led_master_ec: None,
            led_group_bufs: AHashMap::default(),
            led_group_buf_last_used_frame: AHashMap::default(),
            led_group_effect_chains: AHashMap::default(),
            led_group_chain_last_used_frame: AHashMap::default(),
            led_black_tex: None,
            preview_request: None,
            dump_request: None,
            profiling_enabled: false,
            force_serial: false,
        }
    }

    /// Apply the current preview request to every screen-effect chain: the one
    /// holding the watched effect aims its capture at the selected node, all
    /// others clear. Called once per frame before compositing so a freshly
    /// rebuilt chain re-acquires its target. LED chains are excluded — preview
    /// is a screen-authoring aid.
    fn apply_preview_targets(&mut self) {
        let request = self.preview_request.clone();
        let dump = self.dump_request.clone();
        let apply = |chain: &mut Option<PresetRuntime>| {
            if let Some(cg) = chain.as_mut() {
                match &request {
                    Some((effect_id, node_id)) => {
                        cg.set_preview_target(effect_id, node_id.as_ref())
                    }
                    None => cg.clear_preview_target(),
                }
                // Enable the dump only on the chain holding the requested
                // effect. Cmd+D dumps the whole graph; the editor atlas dumps
                // only the visible nodes. The two modes are mutually exclusive,
                // so each clears the other.
                match &dump {
                    Some(crate::compositor::DumpRequest::All(eid)) => {
                        cg.set_dump(Some(eid));
                        cg.clear_dump_set();
                    }
                    Some(crate::compositor::DumpRequest::Visible(eid, nodes)) => {
                        cg.set_dump(None);
                        cg.set_dump_visible(Some(eid), nodes);
                    }
                    None => {
                        cg.set_dump(None);
                        cg.clear_dump_set();
                    }
                }
            }
        };
        apply(&mut self.master_effect_chain);
        for chain in self.effect_chains.values_mut() {
            apply(chain);
        }
        for chain in self.group_effect_chains.values_mut() {
            apply(chain);
        }
    }

    /// Ensure a layer scratch buffer exists for the given `LayerId`,
    /// allocating at the current main compositor resolution if missing.
    /// Stamps last-used so trim keeps it alive. Resolution changes are
    /// handled in `resize`, which walks all bufs.
    fn ensure_layer_buf(
        &mut self,
        layer_id: &LayerId,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
    ) {
        let w = self.main.width();
        let h = self.main.height();
        if !self.layer_bufs.contains_key(layer_id) {
            self.layer_bufs.insert(
                layer_id.clone(),
                PingPong::new(device, pool, w, h, "Layer Scratch"),
            );
        }
        self.layer_buf_last_used_frame
            .insert(layer_id.clone(), self.frame_counter);
    }

    /// Ensure a chain exists for the given `LayerId`. Stable across frames and
    /// layer reorders — the chain's cached `PresetRuntime` (with primitive state)
    /// is preserved as long as the layer is touched within `CHAIN_GRACE_FRAMES`.
    /// Also marks the chain as used this frame so it survives trimming.
    fn ensure_chain_for_layer(&mut self, layer_id: &LayerId) {
        self.effect_chains.entry(layer_id.clone()).or_default();
        self.chain_last_used_frame
            .insert(layer_id.clone(), self.frame_counter);
    }

    /// Same contract as `ensure_chain_for_layer`, but for the group-effect-chain
    /// pool keyed by the group container's `LayerId`.
    fn ensure_group_chain(&mut self, group_id: &LayerId) {
        self.group_effect_chains
            .entry(group_id.clone())
            .or_default();
        self.group_chain_last_used_frame
            .insert(group_id.clone(), self.frame_counter);
    }

    /// Ensure a group scratch buffer exists for the given group's `LayerId`,
    /// allocating at the current main compositor resolution if missing.
    fn ensure_group_buf(
        &mut self,
        group_id: &LayerId,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
    ) {
        let w = self.main.width();
        let h = self.main.height();
        if !self.group_bufs.contains_key(group_id) {
            self.group_bufs.insert(
                group_id.clone(),
                PingPong::new(device, pool, w, h, "Group Scratch"),
            );
        }
        self.group_buf_last_used_frame
            .insert(group_id.clone(), self.frame_counter);
    }

    /// Ensure an LED group scratch buffer exists for the given group's
    /// `LayerId` at the supplied LED grid resolution. Resizes the buffer
    /// (and all other LED group bufs) if the resolution changed since
    /// the previous frame. Stamps last-used for trim.
    fn ensure_led_group_buf(
        &mut self,
        group_id: &LayerId,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        width: u32,
        height: u32,
    ) {
        if !self.led_group_bufs.contains_key(group_id) {
            self.led_group_bufs.insert(
                group_id.clone(),
                PingPong::new(device, pool, width, height, "LED Group Scratch"),
            );
        }
        // If the LED grid changed since last frame, resize every entry
        // (cheap if size already matches).
        for buf in self.led_group_bufs.values_mut() {
            if buf.width() != width || buf.height() != height {
                buf.resize(device, width, height);
            }
        }
        self.led_group_buf_last_used_frame
            .insert(group_id.clone(), self.frame_counter);
    }

    /// Ensure an LED group chain exists for the given `LayerId`.
    /// Internal effect-chain buffers lazy-allocate at LED grid resolution
    /// via the `PresetContext` passed to `apply_effects`.
    fn ensure_led_group_chain(&mut self, group_id: &LayerId) {
        self.led_group_effect_chains
            .entry(group_id.clone())
            .or_default();
        self.led_group_chain_last_used_frame
            .insert(group_id.clone(), self.frame_counter);
    }

    /// Hybrid pool eviction policy:
    ///
    /// 1. **Event-based (immediate)**: drop any pool entry whose `LayerId`
    ///    is no longer in `current_layers`. Fires the moment a layer is
    ///    removed from the project — deterministic, no waiting.
    /// 2. **Time-based (safety net)**: drop entries unused for more than
    ///    `CHAIN_GRACE_FRAMES`. Catches "operator has clearly moved on
    ///    from this section" in multi-hour shows where layers technically
    ///    still exist in the project but won't be revisited.
    ///
    /// Together: brief mutes / clip gaps preserve feedback state for
    /// visual continuity; long idle / layer deletion reclaims memory.
    fn trim_excess_buffers(&mut self, current_layers: &[CompositeLayerDescriptor]) {
        // Build a set of LayerIds present in the project this frame.
        // Group layers are also `CompositeLayerDescriptor`s, so this set
        // covers both per-layer and per-group pool keys uniformly.
        let mut alive: ahash::AHashSet<&LayerId> =
            ahash::AHashSet::with_capacity(current_layers.len());
        for l in current_layers {
            alive.insert(l.layer_id);
        }

        let now = self.frame_counter;
        Self::prune_pool(
            now,
            &alive,
            &mut self.effect_chains,
            &mut self.chain_last_used_frame,
        );
        Self::prune_pool(
            now,
            &alive,
            &mut self.group_effect_chains,
            &mut self.group_chain_last_used_frame,
        );
        Self::prune_pool(
            now,
            &alive,
            &mut self.led_group_effect_chains,
            &mut self.led_group_chain_last_used_frame,
        );
        Self::prune_pool(
            now,
            &alive,
            &mut self.layer_bufs,
            &mut self.layer_buf_last_used_frame,
        );
        Self::prune_pool(
            now,
            &alive,
            &mut self.group_bufs,
            &mut self.group_buf_last_used_frame,
        );
        Self::prune_pool(
            now,
            &alive,
            &mut self.led_group_bufs,
            &mut self.led_group_buf_last_used_frame,
        );
    }

    /// Hybrid pool pruner shared by every chain / buf map.
    /// Drops entries where the `LayerId` is no longer alive in the
    /// project, OR `last_used` is older than `CHAIN_GRACE_FRAMES`.
    /// The stale list `Vec` allocates zero bytes when nothing is stale.
    fn prune_pool<V>(
        now: u64,
        alive: &ahash::AHashSet<&LayerId>,
        pool: &mut AHashMap<LayerId, V>,
        last_used: &mut AHashMap<LayerId, u64>,
    ) {
        let stale: Vec<LayerId> = last_used
            .iter()
            .filter(|(id, last)| {
                !alive.contains(id) || now.saturating_sub(**last) > CHAIN_GRACE_FRAMES
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            pool.remove(&id);
            last_used.remove(&id);
        }
    }

    /// For every effect chain whose layer / group did NOT dispatch this
    /// frame (no active clips, layer muted, or layer outside the solo
    /// set), wipe persistent primitive state — Watercolor feedback,
    /// Bloom mip pyramids, Halation buffers, the legacy adapter's inner
    /// effect's per-owner state, the chain's `StateStore`, etc. The
    /// chain INSTANCE stays alive (managed by `trim_excess_buffers` /
    /// `CHAIN_GRACE_FRAMES`) so reactivation has no rebuild cost — only
    /// the cached state on each node is dropped.
    ///
    /// Matches the live-performance intuition: "if nothing is playing
    /// on this layer right now, the next clip that fires should start
    /// from a clean slate." Idempotent — `EffectNode::clear_state` is a
    /// no-op when state is already cleared.
    ///
    /// Contract for new stateful primitives: override `clear_state` so
    /// it drops every persistent texture / accumulator / mip pyramid
    /// the node owns. The `clear_state` hook is the single integration
    /// point for this policy — implementing it once on the primitive
    /// makes the primitive automatically reset on every layer-idle
    /// transition.
    fn clear_idle_chain_state(&mut self) {
        let now = self.frame_counter;

        // Layer-level chains.
        let last_used = &self.chain_last_used_frame;
        for (id, chain) in self.effect_chains.iter_mut() {
            if last_used.get(id) != Some(&now) {
                clear_chain_state(chain);
            }
        }

        // Group-level chains.
        let last_used = &self.group_chain_last_used_frame;
        for (id, chain) in self.group_effect_chains.iter_mut() {
            if last_used.get(id) != Some(&now) {
                clear_chain_state(chain);
            }
        }

        // LED group chains — separate from screen-path groups so LED-
        // path state can't bleed across pause / mute either.
        let last_used = &self.led_group_chain_last_used_frame;
        for (id, chain) in self.led_group_effect_chains.iter_mut() {
            if last_used.get(id) != Some(&now) {
                clear_chain_state(chain);
            }
        }
    }

    /// Apply effect chain to the given input texture, returning the processed texture
    /// if any effects were applied, or None if the input should be used as-is.
    #[allow(clippy::too_many_arguments)]
    fn apply_effects<'a>(
        effect_chain: &'a mut Option<PresetRuntime>,
        gpu: &mut GpuEncoder,
        input_texture: &'a GpuTexture,
        effects: &[PresetInstance],
        groups: &[EffectGroup],
        ctx: &PresetContext,
        preview_effect: Option<&EffectId>,
        scope: &str,
        profiling: bool,
    ) -> Option<&'a GpuTexture> {
        dispatch_chain(
            effect_chain,
            gpu,
            input_texture,
            effects,
            groups,
            ctx,
            preview_effect,
            scope,
            profiling,
        )
    }

    /// Clean up per-owner effect state for a stopped clip.
    ///
    /// Per-clip state in the graph-runtime path lives inside each chain's
    /// `StateStore`, keyed by `(NodeInstanceId, OwnerKey)`. Today every
    /// chain uses a layer-level owner_key for the chain it owns; clip-
    /// keyed state only exists for short-circuit per-clip stateful nodes
    /// (none today). The legacy `EffectRegistry::cleanup_clip_owner`
    /// call site was deleted along with the legacy dispatcher.
    pub fn cleanup_clip_owner_internal(&mut self, _clip_id: &str) {
        // No-op until a graph-runtime primitive declares per-clip state.
        // See `docs/EFFECT_CHAIN_LIFECYCLE.md`.
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
        // Watched effect (if any) — forces its chain unfused so preview can
        // sample inner node outputs. Owned clone so it survives the raw-pointer
        // borrows of `self.effect_chains` below. Cheap; `None` when no preview.
        let preview_fx = self.preview_request.as_ref().map(|(e, _)| e.clone());

        // Pre-scan: count multi-clip layers and collect the set of
        // active `LayerId`s that need a chain this frame. Effect
        // chains are keyed by `LayerId` (a stable Arc<str> per
        // layer), NOT by iteration order or `layer_index`. This
        // makes the bug class structurally impossible:
        //   - Iteration order changing → still the same key → still
        //     the same EffectChain instance → cached PresetRuntime
        //     survives.
        //   - Layer reordered in timeline (drag-drop) → `layer_index`
        //     shifts but `LayerId` doesn't → same EffectChain → state
        //     preserved.
        //   - Iteration-counter indexing (`chains[counter]`) won't
        //     compile because `Index<usize>` isn't on AHashMap.
        // Pre-scan: collect the set of active `LayerId`s needing a
        // chain this frame, AND the subset needing a layer scratch
        // buf (multi-clip OR has-layer-effects). Both pools are
        // keyed by `LayerId` (a stable Arc<str> per layer), NOT by
        // iteration order or `layer_index`. This makes the
        // positional-indexing bug class structurally impossible:
        //   - Iteration order changing → still the same key → still
        //     the same EffectChain instance → cached PresetRuntime
        //     survives.
        //   - Layer reordered in timeline (drag-drop) → `layer_index`
        //     shifts but `LayerId` doesn't → same EffectChain → state
        //     preserved.
        //   - Iteration-counter indexing (`chains[counter]`) won't
        //     compile because `Index<usize>` isn't on AHashMap.
        self.active_layer_ids_scratch.clear();
        self.active_layer_buf_ids_scratch.clear();
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
                if let Some(ld) = layer_desc {
                    self.active_layer_ids_scratch.push(ld.layer_id.clone());
                    if clip_count > 1 || has_layer_effects {
                        self.active_layer_buf_ids_scratch.push(ld.layer_id.clone());
                    }
                }
            }
        }

        // Pre-insert chain entries for every active layer, and buf
        // entries for every multi-clip / has-effects layer. Doing
        // both before the main loop keeps `get_mut` safe inside the
        // loop (no insertions mid-iteration → no rehash).
        for i in 0..self.active_layer_ids_scratch.len() {
            let id = self.active_layer_ids_scratch[i].clone();
            self.ensure_chain_for_layer(&id);
        }
        for i in 0..self.active_layer_buf_ids_scratch.len() {
            let id = self.active_layer_buf_ids_scratch[i].clone();
            self.ensure_layer_buf(&id, gpu.device, gpu.pool);
        }

        self.layer_outputs_scratch.clear();
        self.clip_post_fx_scratch.clear();

        // Split-borrow: take disjoint &muts so safe `get_mut` on
        // each map can coexist with `self.blend.blend_pass` /
        // `apply_effects` calls below — the borrow checker sees
        // these as disjoint field accesses.
        let chains = &mut self.effect_chains;
        let layer_bufs = &mut self.layer_bufs;

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

            // Render-skip: hidden behind a full-opacity Opaque layer and safe
            // to not render at all (content pipeline's render_skip set). Push
            // no LayerOutput — the layer is neither rendered nor blended. Only
            // plain top-level non-LED leaves land here, so groups/LED are
            // unaffected (they stay on the blend-skip-only path).
            if frame.render_skip.contains(&layer_idx) {
                continue;
            }

            // Check if this layer has layer-level effects
            let has_layer_effects = layer_desc.is_some_and(|ld| has_enabled_effects(ld.effects));

            if group.len() == 1 && !has_layer_effects {
                // Single clip with NO layer effects — pass texture straight through.
                // No chain access needed.
                let clip = &group[0];

                self.layer_outputs_scratch.push(LayerOutput {
                    texture: clip.texture,
                    blend_mode: layer_blend,
                    opacity: layer_opacity * clip.opacity,
                    layer_index: layer_idx,
                    blit_to_led: layer_desc.is_some_and(|ld| ld.blit_to_led),
                });
            } else {
                // Multi-clip or layer-effects: composite into layer buffer.
                // Pools are keyed by LayerId, so a layer without a
                // descriptor (degenerate state — clips referencing a
                // layer_index that doesn't exist in `frame.layers`)
                // is skipped here. In the previous Vec-keyed scheme
                // such layers got a default-blend composite; now
                // there's no LayerId to key the buf, so we drop the
                // output entirely. In practice `frame.layers` is the
                // authoritative source, so this branch is unreachable.
                let Some(ld) = layer_desc else {
                    continue;
                };
                let layer_id = ld.layer_id;
                let layer_buf = layer_bufs
                    .get_mut(layer_id)
                    .expect("buf pre-inserted in active scan");

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
                    let effect_chain = chains
                        .get_mut(ld.layer_id)
                        .expect("chain pre-inserted in active scan");
                    let ctx = PresetContext {
                        time: frame.time,
                        beat: frame.beat,
                        dt: frame.dt,
                        width,
                        height,
                        output_width: frame.output_width,
                        output_height: frame.output_height,
                        aspect: if height > 0 {
                            width as f32 / height as f32
                        } else {
                            1.0
                        },
                        owner_key: layer_id_owner_key(ld.layer_id),
                        is_clip_level: false,
                        frame_count: frame.frame_count as i64,
                        anim_progress: 0.0,
                        trigger_count: ld.trigger_count,
                    };
                    Self::apply_effects(
                        effect_chain,
                        gpu,
                        layer_buf.source_texture(),
                        ld.effects,
                        ld.effect_groups,
                        &ctx,
                        preview_fx.as_ref(),
                        &fx_scope(ld.layer_id),
                        self.profiling_enabled,
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
                // §24 5c: for a SINGLE-clip layer, this post-effect output IS that
                // clip's full look — expose it for a with-effects thumbnail. Clone
                // (cheap retain) now, while the target is provably alive, so the
                // snapshot later in the frame binds a live texture even if the pool
                // recycled the original.
                if group.len() == 1 {
                    // Safety: `effective_layer_tex` was just produced this iteration
                    // and is alive here; we clone immediately rather than store the
                    // pointer for later deref.
                    let texture = unsafe { (*effective_layer_tex).clone() };
                    self.clip_post_fx_scratch.push(ClipPostFx {
                        clip_id: group[0].clip_id.to_string(),
                        texture,
                    });
                }
            }
        }
    }

    /// Phase B: Blend all layer outputs into main in order.
    ///
    /// Layers are blended bottom-to-top (order preserved from generate_layers).
    /// This pass is always serial — each blend reads the previous blend's result.
    ///
    /// `occluded_layers` (from `CompositorFrame`) lists layers hidden by a
    /// fully-opaque layer above them: their blend dispatch is skipped because
    /// the opaque blend overwrites every pixel anyway. Strictly an elision of
    /// redundant math — the layers themselves rendered normally upstream.
    fn blend_layers(
        &mut self,
        gpu: &mut GpuEncoder,
        layer_outputs: &[LayerOutput],
        occluded_layers: &[i32],
    ) {
        // Clear main to opaque black
        self.main.clear_source(gpu, true);

        for output in layer_outputs {
            if occluded_layers.contains(&output.layer_index) {
                continue;
            }
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
                        // Both pools keyed by the group's own LayerId —
                        // stable across iteration order and timeline reorders.
                        self.ensure_led_group_buf(group.layer_id, gpu.device, gpu.pool, w, h);
                        self.ensure_led_group_chain(group.layer_id);
                        let group_buf_ptr = self
                            .led_group_bufs
                            .get_mut(group.layer_id)
                            .expect("ensured above")
                            as *mut PingPong;
                        let group_ec_ptr = self
                            .led_group_effect_chains
                            .get_mut(group.layer_id)
                            .expect("ensured above")
                            as *mut Option<PresetRuntime>;
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
                        let group_source: *const GpuTexture = if has_enabled_effects(group.effects)
                        {
                            let ctx = PresetContext {
                                time: frame.time,
                                beat: frame.beat,
                                dt: frame.dt,
                                width: w,
                                height: h,
                                output_width: frame.output_width,
                                output_height: frame.output_height,
                                aspect: if h > 0 { w as f32 / h as f32 } else { 1.0 },
                                owner_key: led_group_owner_key(group.layer_id),
                                is_clip_level: false,
                                frame_count: frame.frame_count as i64,
                                anim_progress: 0.0,
                                trigger_count: 0,
                            };
                            match Self::apply_effects(
                                group_ec,
                                gpu,
                                group_buf.source_texture(),
                                group.effects,
                                group.effect_groups,
                                &ctx,
                                // LED path is not a preview surface — never unfuse for it.
                                None,
                                &led_scope(group.layer_id),
                                self.profiling_enabled,
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
                    (layer_outputs[i].blend_mode, unsafe { &*black_tex_ptr })
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
        // Watched effect id for the group-FX build gate (forces it unfused so
        // preview can sample inner outputs). Owned clone to avoid re-borrowing
        // `self` inside the group loop. `None` when no preview is active.
        let preview_fx = self.preview_request.as_ref().map(|(e, _)| e.clone());
        // Early exit: no groups → nothing to fold
        if !frame.layers.iter().any(|l| l.is_group) {
            return;
        }

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

            // Allocate this group's buf keyed by its LayerId.
            self.ensure_group_buf(group_desc.layer_id, gpu.device, gpu.pool);
            let group_id = group_desc.layer_id;
            // Take a raw pointer so we can hold &mut group_buf across
            // the inner call to `self.ensure_group_chain` (which
            // would otherwise conflict with a live mut borrow on
            // self.group_bufs). Safety: the pool isn't resized
            // again within this iteration body.
            let group_buf_ptr =
                self.group_bufs.get_mut(group_id).expect("ensured above") as *mut PingPong;
            let group_buf = unsafe { &mut *group_buf_ptr };

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
            let group_texture: *const GpuTexture = if has_enabled_effects(group_desc.effects) {
                // Each group's chain is keyed by its own LayerId —
                // stable across iteration order and timeline reorders.
                self.ensure_group_chain(group_id);
                let effect_chain = self
                    .group_effect_chains
                    .get_mut(group_id)
                    .expect("ensured above");
                let main_w = self.main.width();
                let main_h = self.main.height();
                let ctx = PresetContext {
                    time: frame.time,
                    beat: frame.beat,
                    dt: frame.dt,
                    width: main_w,
                    height: main_h,
                    output_width: frame.output_width,
                    output_height: frame.output_height,
                    aspect: if main_h > 0 {
                        main_w as f32 / main_h as f32
                    } else {
                        1.0
                    },
                    owner_key: group_id_owner_key(group_id),
                    is_clip_level: false,
                    frame_count: frame.frame_count as i64,
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let result = Self::apply_effects(
                    effect_chain,
                    gpu,
                    group_buf.source_texture(),
                    group_desc.effects,
                    group_desc.effect_groups,
                    &ctx,
                    preview_fx.as_ref(),
                    &fx_scope(group_id),
                    self.profiling_enabled,
                );
                result.map_or(group_buf.source_texture() as *const _, |t| t as *const _)
            } else {
                group_buf.source_texture() as *const _
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
        let pre_fold_outputs =
            unsafe { std::slice::from_raw_parts(pre_fold_outputs_ptr, pre_fold_outputs_len) };
        self.blend_layers_to_led(gpu, pre_fold_outputs, frame);

        self.fold_groups(gpu, frame);
        // Safety: layer_outputs_scratch contains raw pointers to textures owned
        // by effect chains, layer bufs, or clip render targets — all valid for
        // the frame duration. Using a raw pointer avoids a split-borrow conflict
        // with blend_layers (which also needs &mut self for main ping-pong).
        let outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let outputs_len = self.layer_outputs_scratch.len();
        let outputs = unsafe { std::slice::from_raw_parts(outputs_ptr, outputs_len) };
        self.blend_layers(gpu, outputs, frame.occluded_layers);
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

        // Watched effect (if any) — owned clone usable inside the per-layer
        // loop below without re-borrowing `self`. Forces its chain unfused so
        // the authoring-time preview can sample inner node outputs.
        let preview_fx = self.preview_request.as_ref().map(|(e, _)| e.clone());

        self.uniform_arena.reset();

        // Ensure async event exists
        if self.async_event.is_none() {
            self.async_event = Some(device.create_event());
        }
        // Raw pointer to avoid borrow conflict with &mut self later.
        // Safety: async_event lives for the duration of this method and
        // is not modified (only signal values change, which is interior mutation).
        let async_event: *const manifold_gpu::GpuEvent = self.async_event.as_ref().unwrap();

        // Pre-scan: see `generate_layers` for the structural
        // rationale behind keying chains + bufs by `LayerId`.
        self.active_layer_ids_scratch.clear();
        self.active_layer_buf_ids_scratch.clear();
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
                if let Some(ld) = layer_desc {
                    self.active_layer_ids_scratch.push(ld.layer_id.clone());
                    if clip_count > 1 || has_layer_effects {
                        self.active_layer_buf_ids_scratch.push(ld.layer_id.clone());
                    }
                }
            }
        }

        // Pre-insert all needed chain + buf entries.
        for i in 0..self.active_layer_ids_scratch.len() {
            let id = self.active_layer_ids_scratch[i].clone();
            self.ensure_chain_for_layer(&id);
        }
        for i in 0..self.active_layer_buf_ids_scratch.len() {
            let id = self.active_layer_buf_ids_scratch[i].clone();
            self.ensure_layer_buf(&id, device, pool);
        }

        // Split-borrow: disjoint &muts so safe `get_mut(id)` works
        // inside the loop without fighting the borrow checker.
        let chains = &mut self.effect_chains;
        let layer_bufs = &mut self.layer_bufs;
        let base_signal = self.async_signal_base;

        self.layer_outputs_scratch.clear();
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

            // Render-skip (parallel path mirror of `generate_layers`): skip
            // hidden-behind-opaque leaves entirely — no encoder, no output.
            if frame.render_skip.contains(&layer_idx) {
                continue;
            }

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
                    // See `generate_layers` for the no-descriptor rationale.
                    let Some(ld) = layer_desc else {
                        continue;
                    };
                    let layer_id = ld.layer_id;
                    let layer_buf = layer_bufs
                        .get_mut(layer_id)
                        .expect("buf pre-inserted in active scan");

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
                        // Look up this layer's chain by LayerId. Pre-inserted
                        // above, so unwrap is safe.
                        let effect_chain = chains
                            .get_mut(ld.layer_id)
                            .expect("chain pre-inserted in active scan");
                        let ctx = PresetContext {
                            time: frame.time,
                            beat: frame.beat,
                            dt: frame.dt,
                            width,
                            height,
                            output_width: frame.output_width,
                            output_height: frame.output_height,
                            aspect: if height > 0 {
                                width as f32 / height as f32
                            } else {
                                1.0
                            },
                            owner_key: layer_id_owner_key(ld.layer_id),
                            is_clip_level: false,
                            frame_count: frame.frame_count as i64,
                            anim_progress: 0.0,
                            trigger_count: ld.trigger_count,
                        };
                        Self::apply_effects(
                            effect_chain,
                            &mut gpu,
                            layer_buf.source_texture(),
                            ld.effects,
                            ld.effect_groups,
                            &ctx,
                            preview_fx.as_ref(),
                            &fx_scope(ld.layer_id),
                            self.profiling_enabled,
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
        let pre_fold_outputs =
            unsafe { std::slice::from_raw_parts(pre_fold_outputs_ptr, pre_fold_outputs_len) };
        self.blend_layers_to_led(compositor_gpu, pre_fold_outputs, frame);

        // Fold group children into single outputs before blending.
        self.fold_groups(compositor_gpu, frame);

        // Serial blend phase on the compositor command buffer.
        // Safety: layer_outputs_scratch is populated above and not modified during blend.
        let outputs_ptr = self.layer_outputs_scratch.as_ptr();
        let outputs_len = self.layer_outputs_scratch.len();
        let outputs = unsafe { std::slice::from_raw_parts(outputs_ptr, outputs_len) };
        self.blend_layers(compositor_gpu, outputs, frame.occluded_layers);
    }
}

impl Compositor for LayerCompositor {
    /// §24 5c with-effects thumbnails — the post-effect output for a sole-clip
    /// layer (that clip's full look). `None` for multi-clip layers; valid only for
    /// the frame just rendered. See the trait default for the contract.
    fn clip_post_fx_texture(&self, clip_id: &str) -> Option<&GpuTexture> {
        self.clip_post_fx_scratch
            .iter()
            .find(|c| c.clip_id == clip_id)
            // Owns a live clone of the post-fx target (see `ClipPostFx` doc), so no
            // raw-pointer deref and no risk of the target being recycled first.
            .map(|c| &c.texture)
    }

    /// Set (or clear) the authoring-time node-output preview request. Cheap;
    /// applied to every screen chain by `apply_preview_targets` each frame.
    fn set_preview_request(&mut self, request: Option<(EffectId, Option<NodeId>)>) {
        self.preview_request = request;
    }

    /// The captured preview texture for this frame, if a preview is active and
    /// its node produced one. Scans the screen-effect chains and returns the
    /// first match (only the chain holding the watched effect captures).
    fn preview_texture(&self) -> Option<&GpuTexture> {
        // No preview active → nothing to read.
        self.preview_request.as_ref()?;
        self.master_effect_chain
            .as_ref()
            .and_then(|cg| cg.preview_texture())
            .or_else(|| {
                self.effect_chains
                    .values()
                    .filter_map(|c| c.as_ref().and_then(|cg| cg.preview_texture()))
                    .next()
            })
            .or_else(|| {
                self.group_effect_chains
                    .values()
                    .filter_map(|c| c.as_ref().and_then(|cg| cg.preview_texture()))
                    .next()
            })
    }

    /// Encoding for this frame's previewed node. Walks the same chains as
    /// [`Self::preview_texture`] and returns the watched chain's encoding.
    fn preview_encoding(&self) -> crate::node_graph::PreviewEncoding {
        if self.preview_request.is_none() {
            return crate::node_graph::PreviewEncoding::Color;
        }
        if let Some(cg) = self
            .master_effect_chain
            .as_ref()
            .filter(|cg| cg.preview_texture().is_some())
        {
            return cg.preview_encoding();
        }
        for chain in self
            .effect_chains
            .values()
            .chain(self.group_effect_chains.values())
        {
            if let Some(cg) = chain.as_ref().filter(|cg| cg.preview_texture().is_some()) {
                return cg.preview_encoding();
            }
        }
        crate::node_graph::PreviewEncoding::Color
    }

    /// Live scalar I/O of this frame's previewed node, for the value inspector.
    /// Walks the watched chain regardless of whether it captured a texture —
    /// the inspector is exactly the no-texture case.
    fn preview_scalar_io(&self) -> crate::node_graph::PreviewScalarIo {
        if self.preview_request.is_none() {
            return (Vec::new(), Vec::new());
        }
        if let Some(cg) = self.master_effect_chain.as_ref() {
            let io = cg.preview_scalar_io();
            if !io.0.is_empty() || !io.1.is_empty() {
                return io;
            }
        }
        for chain in self
            .effect_chains
            .values()
            .chain(self.group_effect_chains.values())
        {
            if let Some(cg) = chain.as_ref() {
                let io = cg.preview_scalar_io();
                if !io.0.is_empty() || !io.1.is_empty() {
                    return io;
                }
            }
        }
        (Vec::new(), Vec::new())
    }

    /// Live param values for every node of the watched effect. The watched id
    /// comes from `preview_request`; we ask each chain for that effect's nodes
    /// and return the first non-empty (only the chain holding the effect has a
    /// matching slot, so at most one answers). Same chain set as
    /// [`Self::preview_scalar_io`] — master, then layer, then group chains.
    fn live_node_params(&self) -> crate::node_graph::LiveNodeParams {
        let Some((effect_id, _)) = self.preview_request.as_ref() else {
            return Vec::new();
        };
        if let Some(cg) = self.master_effect_chain.as_ref() {
            let params = cg.live_node_params(effect_id);
            if !params.is_empty() {
                return params;
            }
        }
        for chain in self
            .effect_chains
            .values()
            .chain(self.group_effect_chains.values())
        {
            if let Some(cg) = chain.as_ref() {
                let params = cg.live_node_params(effect_id);
                if !params.is_empty() {
                    return params;
                }
            }
        }
        Vec::new()
    }

    fn set_dump_request(&mut self, request: Option<crate::compositor::DumpRequest>) {
        self.dump_request = request;
    }

    /// PERF_BUDGET_GATE_DESIGN P2 / D6: fan out to every chain this
    /// compositor currently owns (screen, group, LED-group, master, LED
    /// master), applying the profiling flag + this chain's instance-identity
    /// scope. New chains built later this frame (or on a future frame) get
    /// the same treatment at dispatch time via `apply_effects`'s `scope`/
    /// `profiling` args (chain_dispatch.rs) — this walker only needs to
    /// reach chains that already exist so a mid-run toggle doesn't skip them.
    fn set_profiling(&mut self, on: bool) {
        self.profiling_enabled = on;
        for (id, chain) in self.effect_chains.iter_mut() {
            if let Some(cg) = chain.as_mut() {
                cg.set_profiling(on);
                cg.set_profile_scope(&fx_scope(id));
            }
        }
        for (id, chain) in self.group_effect_chains.iter_mut() {
            if let Some(cg) = chain.as_mut() {
                cg.set_profiling(on);
                cg.set_profile_scope(&fx_scope(id));
            }
        }
        for (id, chain) in self.led_group_effect_chains.iter_mut() {
            if let Some(cg) = chain.as_mut() {
                cg.set_profiling(on);
                cg.set_profile_scope(&led_scope(id));
            }
        }
        if let Some(cg) = self.master_effect_chain.as_mut() {
            cg.set_profiling(on);
            cg.set_profile_scope("master");
        }
        if let Some(Some(cg)) = self.led_master_ec.as_mut() {
            cg.set_profiling(on);
            cg.set_profile_scope("led:master");
        }
    }

    /// D6 correction: profiled mode needs one shared compositor command
    /// buffer to attach the dispatch sampler to; `composite_parallel` gives
    /// each layer its own. Checked at the serial-vs-parallel decision point.
    fn set_force_serial(&mut self, on: bool) {
        self.force_serial = on;
    }

    fn take_step_profiles(&mut self) -> Vec<crate::node_graph::StepProfile> {
        let mut out = Vec::new();
        for chain in self.effect_chains.values_mut().flatten() {
            out.extend(chain.take_step_profiles());
        }
        for chain in self.group_effect_chains.values_mut().flatten() {
            out.extend(chain.take_step_profiles());
        }
        for chain in self.led_group_effect_chains.values_mut().flatten() {
            out.extend(chain.take_step_profiles());
        }
        if let Some(cg) = self.master_effect_chain.as_mut() {
            out.extend(cg.take_step_profiles());
        }
        if let Some(Some(cg)) = self.led_master_ec.as_mut() {
            out.extend(cg.take_step_profiles());
        }
        out
    }

    fn dump_textures(&self) -> Vec<crate::compositor::DumpTextureRef<'_>> {
        let Some(effect_id) = self.dump_request.as_ref().map(|r| r.effect_id()) else {
            return Vec::new();
        };
        // The watched effect lives in exactly one screen chain; find it and
        // pull that effect's captured node outputs.
        let chains = std::iter::once(&self.master_effect_chain)
            .chain(self.effect_chains.values())
            .chain(self.group_effect_chains.values());
        for chain in chains {
            if let Some(cg) = chain.as_ref() {
                let dumped = cg.dump_textures(effect_id);
                if !dumped.is_empty() {
                    return dumped;
                }
            }
        }
        Vec::new()
    }

    fn dump_arrays(&self) -> Vec<crate::compositor::ArrayDump<'_>> {
        let Some(effect_id) = self.dump_request.as_ref().map(|r| r.effect_id()) else {
            return Vec::new();
        };
        let chains = std::iter::once(&self.master_effect_chain)
            .chain(self.effect_chains.values())
            .chain(self.group_effect_chains.values());
        for chain in chains {
            if let Some(cg) = chain.as_ref() {
                let dumped = cg.dump_arrays(effect_id);
                if !dumped.is_empty() {
                    return dumped;
                }
            }
        }
        Vec::new()
    }

    fn render(&mut self, gpu: &mut GpuEncoder, frame: &CompositorFrame) -> &GpuTexture {
        // Aim the authoring-time output preview at the watched node (or clear)
        // before any chain runs, so a freshly rebuilt chain re-acquires it.
        self.apply_preview_targets();
        // Owned clone of the watched effect id for the master-FX build gate
        // below (forces it unfused). Computed before the `&mut self` chain
        // borrows so it doesn't conflict.
        let preview_fx = self.preview_request.as_ref().map(|(e, _)| e.clone());
        if frame.clips.is_empty() {
            // Unity: CompositorStack.cs returns immediately for empty playback.
            // Clear to black + return tonemap output (already cleared from previous frame).
            // Skips ALL master effects, tonemap, and LED tap — zero GPU draw calls.
            gpu.clear_texture(self.main.source_texture(), 0.0, 0.0, 0.0, 1.0);
            self.tonemap.clear(gpu);
            // No layers active — advance frame counter so chains age
            // toward CHAIN_GRACE_FRAMES, then trim stale entries.
            self.frame_counter = self.frame_counter.wrapping_add(1);
            // Pass `frame.layers` so chains for layers that still exist
            // in the project survive (even if the frame has no clips).
            // Layers removed from the project get their chains dropped.
            self.trim_excess_buffers(frame.layers);
            // Frame had zero active clips — every retained chain is by
            // definition idle this frame, so this wipes all per-chain
            // feedback state and primitive accumulators in one pass.
            self.clear_idle_chain_state();
            // Release LED composite resources (nothing to route).
            self.led_main = None;
            self.led_master_ec = None;
            return &self.tonemap.output.texture;
        }

        // Advance frame counter so chain-liveness tracking ages stale
        // entries even when an active layer's chain hasn't been touched
        // this frame.
        self.frame_counter = self.frame_counter.wrapping_add(1);

        // Choose serial vs parallel composite path.
        // Parallel path creates per-layer command buffers for GPU-concurrent
        // generation. Only activated with 2+ active layers (no overhead for
        // single-layer frames).
        let any_solo = frame.layers.iter().any(|l| l.is_solo);
        #[cfg(target_os = "macos")]
        {
            let active_layers = count_active_layers(frame, any_solo);
            // D6 correction: profiled mode forces serial so there is one
            // compositor command buffer to attach the dispatch sampler to —
            // composite_parallel gives each layer its own command buffer,
            // which the sampler can't span.
            if active_layers >= 2 && !self.force_serial {
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

            let ctx = PresetContext {
                time: frame.time,
                beat: frame.beat,
                dt: frame.dt,
                width,
                height,
                output_width: frame.output_width,
                output_height: frame.output_height,
                aspect: if height > 0 {
                    width as f32 / height as f32
                } else {
                    1.0
                },
                owner_key: 0,
                is_clip_level: false,
                frame_count: frame.frame_count as i64,
                anim_progress: 0.0,
                trigger_count: frame.master_trigger_count,
            };

            // Master effects use a dedicated `EffectChain` instance,
            // separate from the per-layer `AHashMap<LayerId, _>`. The
            // master pass has no `LayerId` (it operates on the
            // composited scene), so it lives in its own field — the
            // two cannot collide by construction.
            let master_ec = &mut self.master_effect_chain;

            // Feed tonemap output directly into the effect chain — the first
            // effect reads from tonemap.output without copying.
            if let Some(processed) = Self::apply_effects(
                master_ec,
                gpu,
                &self.tonemap.output.texture,
                frame.master_effects,
                frame.master_effect_groups,
                &ctx,
                preview_fx.as_ref(),
                "master",
                self.profiling_enabled,
            ) {
                // Copy processed result back into tonemap output via GPU memcpy.
                // Use the texture `apply_effects` returned directly — under the
                // PresetRuntime fast path, the result lives in the chain graph's
                // backend (not in `master_ec.ping`/`pong`, which stay None for
                // graph-dispatched chains). `source_texture_pub()` would
                // unwrap a None ping in that case.
                gpu.copy_texture_to_texture(processed, &self.tonemap.output.texture, width, height);
            }
        }

        // ── LED composite: master FX (gated by led_exit_index) ──
        // The LED path runs raw HDR end-to-end — no dedicated tonemap stage.
        // The slicer applies `led_gain` + chroma-preserving clip in linear
        // space before the 8-bit DMX clamp. `led_exit_index` still controls
        // master FX:
        //   * `0` (pre-tonemap tap) — skip master FX. Raw blended composite
        //     goes straight to the LED edge-extend pass. Escape hatch for FX
        //     that don't translate to LEDs.
        //   * `-1` (default, post-effects) — apply master FX in HDR.
        //
        // The LED composite is at native LED grid resolution, so master FX
        // cost is negligible.
        if let Some(ref led_main) = self.led_main
            && frame.led_exit_index == -1
            && has_enabled_effects(frame.master_effects)
        {
            let (width, height) = frame.led_composite_size;
            let width = width.max(1);
            let height = height.max(1);

            let led_ec = self.led_master_ec.get_or_insert_with(Option::<PresetRuntime>::default);

            let ctx = PresetContext {
                time: frame.time,
                beat: frame.beat,
                dt: frame.dt,
                width,
                height,
                output_width: frame.output_width,
                output_height: frame.output_height,
                aspect: if height > 0 {
                    width as f32 / height as f32
                } else {
                    1.0
                },
                owner_key: LED_MASTER_OWNER_KEY,
                is_clip_level: false,
                frame_count: frame.frame_count as i64,
                anim_progress: 0.0,
                trigger_count: frame.master_trigger_count,
            };

            // Run master FX directly on raw HDR `led_main`, copy result back
            // into the same source texture so the slicer reads the post-FX
            // composite. Mirrors the screen path's read-from / copy-back-to
            // tonemap.output pattern.
            let led_src_tex_ptr: *const GpuTexture = led_main.source_texture();
            // Safety: led_src_tex_ptr points to led_main.ping/pong which are
            // not reallocated between the apply_effects call and the copy.
            if let Some(processed) = Self::apply_effects(
                led_ec,
                gpu,
                unsafe { &*led_src_tex_ptr },
                frame.master_effects,
                frame.master_effect_groups,
                &ctx,
                // LED path is not a preview surface — never unfuse for it.
                None,
                "led:master",
                self.profiling_enabled,
            ) {
                gpu.copy_texture_to_texture(
                    processed,
                    unsafe { &*led_src_tex_ptr },
                    width,
                    height,
                );
            }
        } else if self.led_main.is_none() {
            // No LED layers active at all — release the LED master-FX state.
            // (`blend_layers_to_led` clears `led_main` when no layer is
            // flagged.) Don't release on mere exit-path-toggle: that caused
            // churn when live MIDI control flipped `led_exit_index` per
            // frame, dropping the master FX PresetRuntime (and its state) on
            // every toggle. Closes audit finding C-1.
            self.led_master_ec = None;
        }
        // If the LED path is active but exit_index == 0 (pre-tonemap tap),
        // master FX sits warm for the next time the user flips exit_index
        // back to -1.

        // Flush uniform arena (recreates buffer if capacity grew).
        // On native path, arena buffer is not read by GPU dispatches (uses inline
        // set_bytes), but we still flush to handle capacity growth.
        self.uniform_arena.flush(gpu.device);

        // Trim pool entries: drop chains for layers that have been
        // removed from the project (immediate, event-based) AND chains
        // unused for more than CHAIN_GRACE_FRAMES (~5 min at 60 fps,
        // memory-hygiene safety net for long shows).
        self.trim_excess_buffers(frame.layers);
        // Wipe persistent primitive state (feedback buffers, Bloom mip
        // pyramids, etc.) on every chain whose layer didn't dispatch
        // this frame. Matches the live-performance intuition: a layer
        // with no active clips should start fresh on its next clip.
        // The chain INSTANCE itself stays alive — only its cached
        // per-effect state is dropped. See `docs/EFFECT_CHAIN_LIFECYCLE.md`.
        self.clear_idle_chain_state();

        &self.tonemap.output.texture
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.main.resize(device, width, height);
        for lb in self.layer_bufs.values_mut() {
            lb.resize(device, width, height);
        }
        self.blend.resize(width, height);
        // Drop cached chain graphs so they rebuild at the new resolution
        // next frame (the underlying graph holds width/height-sized slots).
        for ec in self.effect_chains.values_mut() {
            *ec = None;
        }
        self.master_effect_chain = None;
        for processor in self.plugin_warmups.iter_mut() {
            processor.resize(device, width, height);
        }
        self.tonemap.resize(device, width, height);
        for gb in self.group_bufs.values_mut() {
            gb.resize(device, width, height);
        }
        for ec in self.group_effect_chains.values_mut() {
            *ec = None;
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
        // Single state cache to walk now that the legacy per-effect
        // dispatcher (and its EffectRegistry-singleton state storage)
        // is gone. Primitive state — Watercolor feedback, Bloom mip
        // pyramids, Stylized Feedback history — lives inside each
        // chain's `chain_graph`, both as instance-level data on the
        // primitive nodes themselves and as keyed entries in the
        // chain's per-instance `StateStore`. Walking every pool entry
        // here resets both styles in one pass.
        //
        // See `docs/EFFECT_CHAIN_LIFECYCLE.md`.
        for chain in self.effect_chains.values_mut() {
            clear_chain_state(chain);
        }
        for chain in self.group_effect_chains.values_mut() {
            clear_chain_state(chain);
        }
        for chain in self.led_group_effect_chains.values_mut() {
            clear_chain_state(chain);
        }
        clear_chain_state(&mut self.master_effect_chain);
        if let Some(led_ec) = self.led_master_ec.as_mut() {
            clear_chain_state(led_ec);
        }
    }

    fn flush_all_background_work(&mut self) {
        for processor in self.plugin_warmups.iter_mut() {
            processor.flush_background_work();
        }
    }

    fn led_tap_texture(&self) -> Option<&GpuTexture> {
        self.led_tap.as_ref().map(|t| &t.texture)
    }

    fn led_composite_texture(&self) -> Option<&GpuTexture> {
        // Present only when at least one layer was flagged `blit_to_led` this
        // frame. Returns the raw HDR LED composite (post-master-FX when
        // exit_index == -1 and master FX are enabled; otherwise pre-FX).
        // No tonemap stage — the slicer applies `led_gain` + chroma-preserving
        // clip in linear space before the 8-bit DMX clamp.
        self.led_main.as_ref().map(|l| l.source_texture())
    }

    fn graph_snapshot_for(
        &self,
        type_id: &manifold_core::PresetTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        let view = crate::node_graph::loaded_preset_view_by_id(type_id)?;
        crate::node_graph::snapshot_for_view(view)
    }

    fn outer_routings_for(
        &self,
        type_id: &manifold_core::PresetTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        let Some(view) = crate::node_graph::loaded_preset_view_by_id(type_id) else {
            return Vec::new();
        };
        crate::node_graph::outer_routings_from_view(view)
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_pool_tests {
    //! Regression tests for the LayerId-keyed chain/buf pools.
    //!
    //! The bug class these guard against: positional indexing
    //! (`Vec<EffectChain>` indexed by iteration counter or
    //! `layer_index`) caused chains to be re-bound to different
    //! layers when the active-clip set shifted or layers were
    //! reordered, forcing per-frame `PresetRuntime` rebuilds and
    //! wiping primitive state (Bloom mips, feedback buffers).
    //!
    //! These tests exercise the pool API directly. The structural
    //! invariant is: "same `LayerId` → same `EffectChain` map
    //! entry across frames, regardless of timeline position or
    //! iteration order." If that holds, every field of the
    //! `EffectChain` (including the cached `chain_graph`) survives
    //! by construction.
    use super::*;

    /// Build a minimal compositor. Tiny size keeps GPU costs low; tests
    /// don't render, so resolution doesn't matter.
    fn make_compositor() -> (crate::TestDevice, LayerCompositor) {
        let device = crate::test_device();
        let comp = LayerCompositor::new(&device, 64, 64);
        (device, comp)
    }

    /// Reserve capacity high enough that test insertions don't trigger
    /// an `AHashMap` rehash — preserves entry pointers for identity
    /// comparison.
    fn reserve_test_capacity(comp: &mut LayerCompositor) {
        comp.effect_chains.reserve(16);
        comp.chain_last_used_frame.reserve(16);
    }

    /// Build a minimal `CompositeLayerDescriptor` for tests that need to
    /// drive `trim_excess_buffers`. All defaults are inert (no clips,
    /// no effects, no group).
    fn make_layer_desc<'a>(
        layer_id: &'a LayerId,
        layer_index: i32,
    ) -> CompositeLayerDescriptor<'a> {
        CompositeLayerDescriptor {
            layer_index,
            layer_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            is_solo: false,
            blit_to_led: false,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        }
    }

    #[test]
    fn chain_entry_stable_across_active_set_changes() {
        // Mirrors the live bug: active layer set shifts frame-to-frame
        // (clips firing/stopping). Each LayerId's chain entry must
        // survive intact regardless of which other layers are active.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let a = LayerId::from("A");
        let b = LayerId::from("B");
        let c = LayerId::from("C");

        // Frame 1: A + B active.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&a);
        comp.ensure_chain_for_layer(&b);
        let a_ptr = comp.effect_chains.get(&a).unwrap() as *const Option<PresetRuntime>;
        let b_ptr = comp.effect_chains.get(&b).unwrap() as *const Option<PresetRuntime>;

        // Frame 2: B + C active (A goes quiet, C new).
        comp.frame_counter = 2;
        comp.ensure_chain_for_layer(&b);
        comp.ensure_chain_for_layer(&c);

        // B is the same instance — its chain_graph, primitive state,
        // and all internal buffers are preserved.
        assert_eq!(
            comp.effect_chains.get(&b).unwrap() as *const Option<PresetRuntime>,
            b_ptr,
            "B's chain instance must be identical across frame transition",
        );
        // A is still in the pool (within grace period).
        assert_eq!(
            comp.effect_chains.get(&a).unwrap() as *const Option<PresetRuntime>,
            a_ptr,
            "A's chain instance must persist within CHAIN_GRACE_FRAMES",
        );

        // Frame 3: A + B + C all active again.
        comp.frame_counter = 3;
        comp.ensure_chain_for_layer(&a);
        comp.ensure_chain_for_layer(&b);
        comp.ensure_chain_for_layer(&c);

        // All entries still the same instances.
        assert_eq!(comp.effect_chains.get(&a).unwrap() as *const _, a_ptr);
        assert_eq!(comp.effect_chains.get(&b).unwrap() as *const _, b_ptr);
    }

    #[test]
    fn chain_entry_independent_of_layer_index() {
        // The original Vec<EffectChain> indexed by `layer_index as usize`
        // would have bound chains to timeline positions; dragging a layer
        // up/down the timeline would have shuffled which chain each layer
        // received. LayerId keying makes that impossible: only the id
        // matters, regardless of `layer_index`.
        //
        // We can't reorder layers without going through the full render
        // pipeline, but we can prove the structural invariant directly:
        // ensure_chain_for_layer takes a LayerId, never a layer_index.
        // The map is `AHashMap<LayerId, EffectChain>` — `chains[5]`
        // (a `usize` index) doesn't compile.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let x = LayerId::from("X");
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&x);
        let x_ptr = comp.effect_chains.get(&x).unwrap() as *const Option<PresetRuntime>;

        // Simulate many frames of reorder activity: ensure many other
        // layers come/go but X stays present.
        for f in 2..20 {
            comp.frame_counter = f;
            // "Other layers at varying timeline positions" — irrelevant
            // because keying is by LayerId, not position.
            let other = LayerId::from(format!("other-{f}"));
            comp.ensure_chain_for_layer(&other);
            comp.ensure_chain_for_layer(&x);
        }

        // X's chain is still the same instance.
        assert_eq!(
            comp.effect_chains.get(&x).unwrap() as *const Option<PresetRuntime>,
            x_ptr,
            "X's chain instance must survive arbitrary other-layer churn",
        );
    }

    #[test]
    fn master_chain_is_separate_field_from_layer_chains() {
        // The master FX pass operates on the composited scene — it has
        // no `LayerId` to key by, so it lives in a dedicated field.
        // This makes "master chain accidentally bound to layer N's chain"
        // structurally impossible: different types, different fields.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let any_layer = LayerId::from("any");
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&any_layer);

        let layer_chain_ptr = comp.effect_chains.get(&any_layer).unwrap() as *const Option<PresetRuntime>;
        let master_chain_ptr: *const Option<PresetRuntime> = &comp.master_effect_chain;

        assert_ne!(
            layer_chain_ptr, master_chain_ptr,
            "master_effect_chain must be a different instance from any layer chain",
        );
    }

    #[test]
    fn chain_dropped_immediately_when_layer_removed_from_project() {
        // Event-based eviction: when a layer disappears from
        // `frame.layers` (project edit removed it), its chain drops on
        // the next `trim_excess_buffers` call — no waiting for the
        // grace timer. This bounds memory tightly to the project's
        // current layer set.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let kept = LayerId::from("kept");
        let removed = LayerId::from("removed");

        // Frame 1: both layers exist and touch their chains.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&kept);
        comp.ensure_chain_for_layer(&removed);
        assert!(comp.effect_chains.contains_key(&kept));
        assert!(comp.effect_chains.contains_key(&removed));

        // Frame 2: the user deletes `removed` from the project. The next
        // CompositorFrame includes only `kept` in its `layers` slice.
        // Even though `removed`'s chain was just touched, trim drops it
        // immediately — the layer no longer exists.
        comp.frame_counter = 2;
        let layers_after_delete = vec![make_layer_desc(&kept, 0)];
        comp.trim_excess_buffers(&layers_after_delete);

        assert!(
            !comp.effect_chains.contains_key(&removed),
            "chain for a deleted layer must drop on the next trim, not wait for grace",
        );
        assert!(
            comp.effect_chains.contains_key(&kept),
            "chain for a layer still in the project must survive",
        );
    }

    #[test]
    fn aged_chains_pruned_after_grace_period() {
        // Timer-based safety net: a chain whose layer is still in the
        // project but hasn't been touched in CHAIN_GRACE_FRAMES is
        // dropped. Catches the "operator moved on from this section
        // hours ago" case in long live shows.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let stale = LayerId::from("stale");
        let alive = LayerId::from("alive");

        // Frame 1: both active and touch their chains.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&stale);
        comp.ensure_chain_for_layer(&alive);

        // Advance well past the grace window while only refreshing `alive`.
        // BOTH layers stay in `frame.layers` — only `stale`'s chain is idle.
        let last_frame = CHAIN_GRACE_FRAMES + 50;
        for f in 2..=last_frame {
            comp.frame_counter = f;
            comp.ensure_chain_for_layer(&alive);
        }

        let layers = vec![make_layer_desc(&stale, 0), make_layer_desc(&alive, 1)];
        comp.trim_excess_buffers(&layers);

        assert!(
            !comp.effect_chains.contains_key(&stale),
            "stale chain must have been pruned after exceeding CHAIN_GRACE_FRAMES",
        );
        assert!(
            comp.effect_chains.contains_key(&alive),
            "alive chain must still be present",
        );
    }

    #[test]
    fn chain_survives_layer_idle_within_grace() {
        // Common live-performance case: a layer mutes / has no active
        // clip for a short window (typical mid-song breakdown), then
        // resumes. Its chain — and any feedback state it holds — must
        // survive the gap so the visual look is continuous.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let idle = LayerId::from("idle");

        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&idle);
        let initial_ptr = comp.effect_chains.get(&idle).unwrap() as *const Option<PresetRuntime>;

        // Many frames pass without `idle` being touched, but the layer
        // is still in the project (typical mute / clip-gap scenario).
        // CHAIN_GRACE_FRAMES is 18000 — pick a value well below it.
        let layers = vec![make_layer_desc(&idle, 0)];
        for f in 2..=(CHAIN_GRACE_FRAMES / 4) {
            comp.frame_counter = f;
            comp.trim_excess_buffers(&layers);
        }

        assert_eq!(
            comp.effect_chains.get(&idle).unwrap() as *const _,
            initial_ptr,
            "chain instance must survive layer-idle periods well below grace window",
        );
    }
}
