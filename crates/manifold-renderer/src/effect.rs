use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;

/// Per-frame context for effects.
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
}

/// GPU-aware post-process effect processor.
/// One singleton per EffectType in the registry. Per-owner state (if any)
/// lives inside each processor, keyed by `EffectContext::owner_key`.
pub trait PostProcessEffect: Send {
    fn effect_type(&self) -> EffectType;

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
    );

    /// Clear all temporal state (called on seek to prevent stale trails/feedback).
    fn clear_state(&mut self) {}

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}

/// Extension for effects that maintain per-owner state (e.g., Feedback, Bloom).
pub trait StatefulEffect: PostProcessEffect {
    /// Clear state for a specific owner (e.g., when a clip is removed).
    fn clear_state_for_owner(&mut self, owner_key: i64);

    /// Clean up all resources for a specific owner.
    fn cleanup_owner(&mut self, owner_key: i64);
}
