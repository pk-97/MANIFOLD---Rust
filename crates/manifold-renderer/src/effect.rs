use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;

/// Per-frame context for effects.
pub struct EffectContext {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    pub width: u32,
    pub height: u32,
}

/// GPU-aware post-process effect processor.
/// Phase 5 will add concrete implementations.
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

    /// Clear temporal state (called on seek to prevent stale trails/feedback).
    fn clear_state(&mut self) {}

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
