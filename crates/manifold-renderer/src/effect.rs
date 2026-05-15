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
    /// Global frame counter — equivalent to Unity's Time.frameCount.
    /// Used by BlobTrackingFX to throttle GPU readbacks.
    pub frame_count: i64,
}

/// GPU-aware post-process effect processor.
///
/// Singleton per `EffectTypeId` in [`EffectRegistry`]. After the splice
/// migration only a handful of methods are still load-bearing:
///
/// - `apply` / `clear_state` — called by the monolithic-wrapper
///   primitives (AutoGain, BlobTracking, Infrared, WireframeDepth,
///   QuadMirror) to drive the legacy compute path.
/// - `resize` / `flush_background_work` — called by `EffectRegistry`
///   on render-resolution changes and per export frame respectively.
///
/// Every shipping effect now wires its host params + frame-by-frame
/// dispatch through `ChainSpec`; the trait stays only for the four
/// methods above.
pub trait PostProcessEffect: Send {
    fn effect_type(&self) -> &EffectTypeId;

    /// Apply the effect. Reads source, writes to target.
    /// The caller swaps buffers after each effect.
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
}
