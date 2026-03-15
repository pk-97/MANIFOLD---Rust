use crate::layer_compositor::CompositeClipDescriptor;
use crate::render_target::RenderTarget;
use manifold_core::color::Color;
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
}

/// Trait for compositing layers into a final output.
pub trait Compositor {
    /// Render into the compositor's internal render targets.
    /// Returns the texture view to present.
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
}

/// Phase 3 stub: clears to a color that cycles based on beat position.
/// Kept for testing/fallback.
pub struct ClearColorCompositor {
    ping: RenderTarget,
    pong: RenderTarget,
    use_ping: bool,
}

impl ClearColorCompositor {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        Self {
            ping: RenderTarget::new(device, width, height, format, "Compositor Ping"),
            pong: RenderTarget::new(device, width, height, format, "Compositor Pong"),
            use_ping: true,
        }
    }
}

impl Compositor for ClearColorCompositor {
    fn render(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &CompositorFrame,
    ) -> &wgpu::TextureView {
        let target = if self.use_ping { &self.ping } else { &self.pong };

        // Cycle hue based on beat position — proves engine is ticking
        let hue = (frame.beat * 0.05) % 1.0;
        let color = Color::hsv_to_rgb(hue, 0.7, 0.9);

        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ClearColor Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: color.r as f64,
                        g: color.g as f64,
                        b: color.b as f64,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        self.use_ping = !self.use_ping;

        if !self.use_ping { &self.ping.view } else { &self.pong.view }
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.ping.resize(device, width, height);
        self.pong.resize(device, width, height);
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.ping.width, self.ping.height)
    }
}
