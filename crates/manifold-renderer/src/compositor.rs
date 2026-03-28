use crate::gpu_encoder::GpuEncoder;
use crate::layer_compositor::CompositeClipDescriptor;
use crate::tonemap::TonemapSettings;
use manifold_core::LayerId;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::BlendMode;

/// Per-layer metadata passed to the compositor.
pub struct CompositeLayerDescriptor<'a> {
    pub layer_index: i32,
    pub layer_id: LayerId,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub is_muted: bool,
    pub is_solo: bool,
    pub effects: &'a [EffectInstance],
    pub effect_groups: &'a [EffectGroup],
}

/// Frame context passed to the compositor each tick.
pub struct CompositorFrame<'a> {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    pub frame_count: u64,
    pub compositor_dirty: bool,
    pub clips: &'a [CompositeClipDescriptor<'a>],
    pub layers: &'a [CompositeLayerDescriptor<'a>],
    pub master_effects: &'a [EffectInstance],
    pub master_effect_groups: &'a [EffectGroup],
    /// Tonemap settings for this frame.
    pub tonemap: TonemapSettings,
    /// LED exit path index: 0 = capture pre-tonemap composite for LED output,
    /// -1 = use final output (default).
    pub led_exit_index: i32,
}

/// Trait for compositing layers into a final output.
pub trait Compositor: Send {
    /// Render into the compositor's internal render targets.
    /// Returns the tonemapped output texture.
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
    ) -> &manifold_gpu::GpuTexture;

    /// Resize compositor render targets.
    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32);

    /// Get current output dimensions.
    fn dimensions(&self) -> (u32, u32);

    /// Pre-tonemap HDR output texture.
    fn pre_tonemap_output(&self) -> &manifold_gpu::GpuTexture;

    /// The final compositor output texture (post-tonemap, post-effects).
    fn output_texture(&self) -> &manifold_gpu::GpuTexture;

    /// Clean up per-owner effect state for a stopped clip.
    fn cleanup_clip_owner(&mut self, clip_id: &str);

    /// Clear all temporal effect state (e.g., on export warmup re-seek).
    fn clear_all_effect_state(&mut self);

    /// Flush in-flight background work in all effect processors.
    fn flush_all_background_work(&mut self);

    /// LED tap texture: pre-tonemap composite captured when led_exit_index == 0.
    /// Returns None if exit index is -1.
    fn led_tap_texture(&self) -> Option<&manifold_gpu::GpuTexture>;
}
