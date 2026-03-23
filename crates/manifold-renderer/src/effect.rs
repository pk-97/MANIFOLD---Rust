use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;

/// Per-frame context for effects.
/// Unity ref: EffectContext.cs
pub struct EffectContext {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    pub width: u32,
    pub height: u32,
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
    effect_type: EffectType,
    param_index: usize,
    default: f32,
) -> f32 {
    chain.iter()
        .find(|fx| fx.effect_type() == effect_type && fx.enabled)
        .and_then(|fx| fx.param_values.get(param_index).copied())
        .unwrap_or(default)
}

/// Default skip check: returns true when param[0] <= 0 (effect has no amount).
/// Unity ref: SimpleBlitEffect.cs line 37
pub fn should_skip_default(fx: &EffectInstance) -> bool {
    fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
}

/// GPU-aware post-process effect processor.
/// One singleton per EffectType in the registry. Per-owner state (if any)
/// lives inside each processor, keyed by `EffectContext::owner_key`.
pub trait PostProcessEffect: Send {
    fn effect_type(&self) -> EffectType;

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
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    );

    /// Clear all temporal state (called on seek to prevent stale trails/feedback).
    fn clear_state(&mut self) {}

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}

    /// Clean up per-owner GPU state for a specific owner.
    /// No-op for non-stateful effects. Stateful effects override to release
    /// per-owner textures/buffers (e.g., Feedback, Bloom, PixelSort).
    /// Called when a clip stops to prevent unbounded GPU memory growth.
    fn cleanup_owner_state(&mut self, _owner_key: i64) {}
}

/// Extension for effects that maintain per-owner state (e.g., Feedback, Bloom).
/// Unity ref: IStatefulEffect.cs
pub trait StatefulEffect: PostProcessEffect {
    /// Clear state for a specific owner (e.g., when a clip is removed).
    fn clear_state_for_owner(&mut self, owner_key: i64);

    /// Clean up all resources for a specific owner.
    fn cleanup_owner(&mut self, owner_key: i64);

    /// Release ALL per-owner GPU state. Called during Clear() (stop playback),
    /// ResizeBuffers(), and WarmupShaders().
    /// Unity ref: IStatefulEffect.cs line 18
    fn cleanup_all_owners(&mut self, device: &wgpu::Device);
}
