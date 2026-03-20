use crate::layer_compositor::CompositeClipDescriptor;
use crate::tonemap::TonemapSettings;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::BlendMode;

/// Per-layer metadata passed to the compositor.
pub struct CompositeLayerDescriptor<'a> {
    pub layer_index: i32,
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
}

/// Trait for compositing layers into a final output.
pub trait Compositor {
    /// Render into the compositor's internal render targets.
    /// Returns the tonemapped texture view to present.
    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &CompositorFrame,
    ) -> &wgpu::TextureView;

    /// Resize compositor render targets.
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32);

    /// Get current output dimensions.
    fn dimensions(&self) -> (u32, u32);

    /// Pre-tonemap HDR output. Returns the linear HDR buffer from before
    /// tonemapping was applied. Used by the export pipeline.
    /// Matches Unity CompositorStack.PreTonemapOutput.
    fn pre_tonemap_output(&self) -> &wgpu::TextureView;

    /// The underlying texture of the tonemapped output.
    /// Used by ContentPipeline to copy the compositor result to a double-buffer.
    fn output_texture(&self) -> &wgpu::Texture;
}

