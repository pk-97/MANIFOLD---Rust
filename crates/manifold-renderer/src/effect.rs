use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

/// Per-frame context for effects.
/// Unity ref: EffectContext.cs
pub struct EffectContext {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    /// Render-resolution dimensions (may be < output dims when scaling is active).
    pub width: u32,
    pub height: u32,
    /// Final output dimensions after upscaling. Use these for pixel-count-dependent
    /// logic (texel sizes, block counts, pattern spacing) so effects are
    /// resolution-invariant across render scales.
    pub output_width: u32,
    pub output_height: u32,
    /// Owner key for per-owner state management in stateful effects.
    /// 0 = master, layer_index+1 = layer, hash(clip_id) = clip.
    pub owner_key: i64,
    pub is_clip_level: bool,
    /// Precomputed cross-chain param: EdgeStretch source width.
    /// Unity ref: EffectContext.FindChainParam(EffectType.EdgeStretch, 1, 0.5625f)
    /// Filled by effect_chain before calling apply. Used by VoronoiPrism.
    pub edge_stretch_width: f32,
    /// Global frame counter — equivalent to Unity's Time.frameCount.
    /// Used by BlobTrackingFX to throttle GPU readbacks.
    pub frame_count: i64,
}

/// Find a parameter value from another effect in a chain.
/// Returns the param value if an enabled effect of the given type exists
/// in the chain, otherwise returns the default value.
/// Unity ref: EffectContext.cs FindChainParam()
pub fn find_chain_param(
    chain: &[EffectInstance],
    effect_type: &EffectTypeId,
    param_index: usize,
    default: f32,
) -> f32 {
    chain
        .iter()
        .find(|fx| fx.effect_type() == effect_type && fx.enabled)
        .and_then(|fx| fx.param_values.get(param_index).map(|p| p.value))
        .unwrap_or(default)
}

/// Default skip check: returns true when param[0] <= 0 (effect has no amount).
/// Unity ref: SimpleBlitEffect.cs line 37
pub fn should_skip_default(fx: &EffectInstance) -> bool {
    fx.param_values.first().map(|p| p.value).unwrap_or(0.0) <= 0.0
}

/// GPU-aware post-process effect processor.
/// One singleton per EffectTypeId in the registry. Per-owner state (if any)
/// lives inside each processor, keyed by `EffectContext::owner_key`.
pub trait PostProcessEffect: Send {
    fn effect_type(&self) -> &EffectTypeId;

    /// Returns true when the effect should be skipped entirely (no GPU work,
    /// no buffer swap). The chain checks this BEFORE calling apply().
    /// Unity ref: SimpleBlitEffect.ShouldSkip() — checked by CompositorStack.
    /// Default: skip when param[0] <= 0.
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        should_skip_default(fx)
    }

    /// Apply the effect. Reads source, writes to target.
    /// The caller swaps buffers after each effect.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    );

    /// Clear all temporal state (called on seek to prevent stale trails/feedback).
    fn clear_state(&mut self) {}

    /// Block until any in-flight background work completes.
    /// Called after each export frame to ensure async pipelines (GPU readback →
    /// background worker → result) resolve deterministically. Default: no-op.
    fn flush_background_work(&mut self) {}

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}

    /// Clean up per-owner GPU state for a specific owner.
    /// No-op for non-stateful effects. Stateful effects override to release
    /// per-owner textures/buffers (e.g., Feedback, Bloom, PixelSort).
    /// Called when a clip stops to prevent unbounded GPU memory growth.
    fn cleanup_owner_state(&mut self, _owner_key: i64) {}

    /// Read-only snapshot of this effect's internal node graph, for the
    /// editor UI. Default: `None` — non-graph effects have nothing to
    /// show. Graph-backed effects override this to walk their internal
    /// `Graph` and return a `GraphSnapshot`. Called from the content
    /// thread once per frame while the editor window is open; cost
    /// scales with the graph size, so keep it cheap.
    fn graph_snapshot(&self) -> Option<crate::node_graph::GraphSnapshot> {
        None
    }

    /// Replace this effect's internal graph with one materialized from
    /// `def`. Default: no-op — non-graph effects ignore the call.
    /// Graph-backed effects override to rebuild their `Graph`, plan,
    /// resource lookups, and composite handle from the def, then
    /// invalidate cached render state so the next frame re-allocates
    /// from scratch.
    ///
    /// Called by the chain builder when `EffectInstance.graph` is
    /// `Some(def)` — i.e., the user has overridden the catalog default
    /// topology for this instance. See `docs/NODE_GRAPH_SYSTEM.md`
    /// Phase 1 for the per-card-divergence model.
    fn hydrate_graph(&mut self, _def: &manifold_core::effect_graph_def::EffectGraphDef) {}
}
