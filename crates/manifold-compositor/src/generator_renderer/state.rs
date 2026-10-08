use super::*;

/// Per-clip active state.
pub struct ActiveClip {
    /// Generator renders into this texture at full output resolution.
    pub render_target: RenderTarget,
    pub generator_type: PresetTypeId,
    pub layer_id: LayerId,
    pub layer_index: i32, // positional cache for param lookup in render_all
    pub clip_index: u32,  // positional cache for string_params lookup (avoids linear scan)
    pub anim_progress: f32,
    /// True on the first frame after acquiring a reused render target.
    /// Cleared to opaque black before the generator renders to prevent
    /// stale content from a previous clip/layer leaking through.
    pub needs_clear: bool,
}

impl ActiveClip {
    /// The texture to hand to the compositor.
    pub(super) fn output_texture(&self) -> &manifold_gpu::GpuTexture {
        &self.render_target.texture
    }
}

/// Per-layer generator state. Persists across clips to maintain
/// temporal state (particle positions, attractors, etc.).
pub struct LayerGeneratorState {
    pub generator: Box<PresetRuntime>,
    pub(super) event_owner: Option<manifold_core::EffectId>,
    pub generator_type: PresetTypeId,
    /// The layer's clip-launch edge counter (existing behavior, unconditional
    /// pre-section 8) — bumped in `acquire_clip`, gated by the generator's own
    /// `audio_trigger.mode` wanting `ClipEdge` (section 8 D1; no config = always on,
    /// preserving old-project behavior byte-for-byte).
    pub clip_count: u32,
    /// section 8 D1: bumped once per audio-trigger fire this layer's generator (or
    /// any effect in its chain — D5) is configured to react to, mode-gated at
    /// increment time by the firing instance's own `audio_trigger.mode`
    /// wanting `Transient`. See [`Self::effective_trigger_count`].
    pub(super) audio_count: u32,
    /// `Layer::generator_graph_structure_version` at the time this generator
    /// was constructed. Only a topology change (node/wire add or remove, type
    /// swap, revert) bumps it, so `acquire_clip` / the per-frame sweep rebuild
    /// the generator only when structure actually changed. A value-only edit
    /// (an inner param tweak) bumps the snapshot version instead and is applied
    /// in place — see [`Self::applied_param_version`]. `None` when built from
    /// the bundled preset (no override present at construction time).
    pub(super) override_version: Option<u32>,
    /// `Layer::generator_graph_version` (the snapshot counter, bumped by every
    /// edit) last reflected into the live graph. When it advances without a
    /// structure change, the sweep pushes the new inner-node param values into
    /// the running generator via `apply_inner_param_overrides` and re-bakes
    /// the binding reshapes from the live manifest via
    /// `apply_manifest_reshape` — no rebuild, so sim/particle state survives.
    /// Keyed on `gen_params` presence (not the override graph's) so a
    /// spec-only mapping edit on a catalog-default generator — which bumps
    /// this counter without materializing an override — is seen here.
    /// `None` when the layer had no generator params at construction.
    pub(super) applied_param_version: Option<u32>,
    /// Cached string params from the layer's clips. When a clip provides a
    /// string param (e.g. fontFamily), it's stored here so that subsequent clips
    /// without that key still get the layer's value. This avoids the first-clip
    /// fallback-to-default problem where e.g. text renders in Inter before the
    /// clip with the selected font is reached.
    pub(super) layer_string_defaults: std::collections::BTreeMap<String, String>,
    /// Cached merged string params (defaults + clip overrides). Rebuilt only
    /// when `string_params_dirty` is set (clip start, type change, data_version).
    pub(super) merged_string_params: std::collections::BTreeMap<String, String>,
    /// True when merged_string_params needs to be rebuilt.
    pub(super) string_params_dirty: bool,
    /// Whether this generator was built unfused because its layer was the
    /// watched (open-in-editor) target. Compared against the live watch state in
    /// the rebuild sweeps so opening/closing the editor flips the generator
    /// fused ⇄ unfused — the registry's fuse gate only re-runs on rebuild.
    pub(super) built_watched: bool,
    /// `docs/DEPTH_RELIGHT_DESIGN.md` P5: the "3D Shading" toggle + knobs
    /// last reflected into this generator, as `(relight, RelightParams)`.
    /// The template is synthesized at splice time from the live
    /// `PresetInstance`, not authored into `generator_graph` — so neither
    /// `override_version` nor `applied_param_version` sees a toggle flip or
    /// a knob drag. Compared against the layer's current `gen_params` each
    /// sweep to force exactly the rebuild those values need.
    pub(super) applied_relight: (bool, manifold_core::effects::RelightParams),
}

impl LayerGeneratorState {
    /// section 8 D1: the value fed into `PresetContext.trigger_count` / consuming
    /// graphs' `generator_input.trigger_count` — the layer's clip edge plus
    /// its audio-trigger fires. Wrapping add: a `u32` overflow only after
    /// billions of triggers on one layer in one session, and wrapping (not
    /// saturating) matches the existing clip-count overflow policy.
    pub(super) fn effective_trigger_count(&self) -> u32 {
        self.clip_count.wrapping_add(self.audio_count)
    }
}

/// section 24 5c cold-start: an ISOLATED generator instance + small render target for one
/// PARKED clip's thumbnail. Separate from the live per-layer `layer_generators` so
/// rendering a parked clip's thumbnail can never disturb an active clip's state on
/// the same layer.
pub struct ThumbGen {
    pub runtime: Box<PresetRuntime>,
    pub rt: RenderTarget,
    pub gen_type: PresetTypeId,
    /// A thumbnail is exposed only when preparation and frame encoding are
    /// complete. GPU completion is protected separately by frame retirement.
    pub ready: bool,
    /// Monotonic per-runtime frame counter. Retries continue from the last
    /// attempted frame instead of restarting stateful generators at zero.
    pub frame_count: i64,
    pub last_frame_status: manifold_node_engine::runtime::frame_status::FrameRenderStatus,
}

