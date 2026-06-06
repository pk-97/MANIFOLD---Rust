use crate::gpu_encoder::GpuEncoder;
use crate::preset_context::PresetContext;
use manifold_core::PresetTypeId;
use manifold_core::effects::EffectInstance;

/// GPU-aware post-process effect processor.
///
/// Held by [`crate::plugin_prewarm::PluginPrewarm`] for the three
/// plugin-using effects (BlobTracking, DepthOfField, WireframeDepth).
/// The chain runtime no longer calls `apply` on these handles — chain
/// dispatch goes through the primitive registry — but the
/// `resize` + `flush_background_work` methods are still load-bearing:
/// `LayerCompositor` forwards through every plugin warmup so FFI
/// workers stay in sync with render resolution and finish in-flight
/// work between export frames.
///
/// The monolithic-wrapper primitives (BlobTracking, Infrared,
/// WireframeDepth, QuadMirror) also invoke `apply` /
/// `clear_state` on their held `Box<dyn PostProcessEffect>` to drive
/// the legacy compute path one block at a time.
pub trait PostProcessEffect: Send {
    fn effect_type(&self) -> &PresetTypeId;

    /// Apply the effect. Reads source, writes to target.
    /// The caller swaps buffers after each effect.
    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &PresetContext,
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
