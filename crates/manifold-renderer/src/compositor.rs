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
    /// Tonemap settings for this frame. Matches Unity CompositorStack properties:
    /// TonemapExposure, HDROutputEnabled, PaperWhiteNits, MaxDisplayNits.
    pub tonemap: TonemapSettings,
    /// LED exit path index: 0 = capture pre-tonemap composite for LED output,
    /// -1 = use final output (default). When 0, the compositor copies the
    /// pre-tonemap buffer into a dedicated texture accessible via led_tap_view().
    pub led_exit_index: i32,
}

/// Trait for compositing layers into a final output.
pub trait Compositor: Send {
    /// Render into the compositor's internal render targets.
    /// Returns the tonemapped texture view to present.
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> &wgpu::TextureView;

    /// Resize compositor render targets.
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32);

    /// Get current output dimensions.
    fn dimensions(&self) -> (u32, u32);

    /// Pre-tonemap HDR output. Returns the linear HDR buffer from before
    /// tonemapping was applied. Used by the export pipeline.
    /// Matches Unity CompositorStack.PreTonemapOutput.
    fn pre_tonemap_output(&self) -> &wgpu::TextureView;

    /// The underlying texture of the pre-tonemap output.
    /// Used on the native Metal path where tonemap isn't yet migrated.
    fn pre_tonemap_texture(&self) -> &wgpu::Texture;

    /// The underlying texture of the tonemapped output.
    /// Used by ContentPipeline to copy the compositor result to a double-buffer.
    fn output_texture(&self) -> &wgpu::Texture;

    /// View of the final compositor output (post-tonemap, post-effects).
    /// Used by PQ encoder for HDR export.
    fn output_view(&self) -> &wgpu::TextureView;

    /// Clean up per-owner effect state for a stopped clip.
    fn cleanup_clip_owner(&mut self, clip_id: &str);

    /// LED tap view: pre-tonemap composite captured when led_exit_index == 0.
    /// Returns None if exit index is -1 (use output_view instead).
    fn led_tap_view(&self) -> Option<&wgpu::TextureView>;
}
