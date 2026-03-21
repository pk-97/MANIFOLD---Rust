use std::sync::{Arc, RwLock};

use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::tonemap::TonemapSettings;
use manifold_playback::engine::{PlaybackEngine, TickResult};

/// Thread-safe shared output view. The content thread writes a new view
/// after each swap; the UI thread reads it for blitting to screen.
///
/// Both threads share a single wgpu Device, so TextureViews created by
/// the content thread are directly usable by the UI thread — zero copy.
pub struct SharedOutputView {
    view: RwLock<Option<wgpu::TextureView>>,
    dimensions: RwLock<(u32, u32)>,
}

impl SharedOutputView {
    pub fn new() -> Self {
        Self {
            view: RwLock::new(None),
            dimensions: RwLock::new((1920, 1080)),
        }
    }

    /// Read the current front buffer view (called by UI thread).
    pub fn get_view(&self) -> Option<wgpu::TextureView> {
        self.view.read().unwrap().clone()
    }

    /// Update the front buffer view (called by content thread after swap).
    pub fn set_view(&self, view: wgpu::TextureView) {
        *self.view.write().unwrap() = Some(view);
    }

    /// Update dimensions (called by content thread on resize).
    pub fn set_dimensions(&self, w: u32, h: u32) {
        *self.dimensions.write().unwrap() = (w, h);
    }

    /// Get current output dimensions (called by UI thread for aspect ratio).
    pub fn get_dimensions(&self) -> (u32, u32) {
        *self.dimensions.read().unwrap()
    }
}

/// Output format for double-buffered compositor output.
/// Matches compositor's tonemap output format.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Self-contained content rendering pipeline.
///
/// Owns the compositor and orchestrates GPU rendering of generators + compositing.
/// The PlaybackEngine (which owns GeneratorRenderer) is borrowed for each frame.
///
/// Double-buffered output: content writes to back buffer, swaps on completion.
/// UI always reads from the stable front buffer via SharedOutputView (zero copy —
/// both threads share the same wgpu Device).
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
    /// Double-buffered output textures. UI reads front, content writes to back.
    /// Lazily initialized on first render (needs device + dimensions).
    output_buffers: Option<[RenderTarget; 2]>,
    /// Which buffer is the front (0 or 1). Back is always `1 - front_index`.
    front_index: usize,
    /// Content frame rate tracking (for separate cadence mode).
    content_interval_secs: f64,
    last_content_time: f64,
    /// Shared output view for cross-thread access. The UI thread holds an Arc
    /// to this and reads the front buffer view for blitting.
    shared_output: Arc<SharedOutputView>,
}

impl ContentPipeline {
    pub fn new(compositor: Box<dyn Compositor>) -> Self {
        let shared = Arc::new(SharedOutputView::new());
        Self {
            compositor,
            output_buffers: None,
            front_index: 0,
            content_interval_secs: 1.0 / 60.0,
            last_content_time: 0.0,
            shared_output: shared,
        }
    }

    /// Get a clone of the shared output handle. The UI thread holds this
    /// to read the front buffer view and dimensions.
    pub fn shared_output(&self) -> Arc<SharedOutputView> {
        Arc::clone(&self.shared_output)
    }

    /// Lazily create the double-buffer pair at compositor dimensions.
    fn ensure_output_buffers(&mut self, device: &wgpu::Device) {
        if self.output_buffers.is_some() {
            return;
        }
        let (w, h) = self.compositor.dimensions();
        self.output_buffers = Some([
            RenderTarget::new(device, w, h, OUTPUT_FORMAT, "ContentOutput_Front"),
            RenderTarget::new(device, w, h, OUTPUT_FORMAT, "ContentOutput_Back"),
        ]);
        self.front_index = 0;
    }

    /// Render all generators and composite into the back buffer, then swap.
    ///
    /// After this call, `output_view()` returns the newly rendered frame.
    pub fn render_content(
        &mut self,
        gpu: &GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
    ) {
        self.ensure_output_buffers(&gpu.device);

        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();

        let mut encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame Encoder"),
                });

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // Render generators via downcast (GPU rendering needs queue + encoder)
        for renderer in renderers.iter_mut() {
            if let Some(gen) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                gen.render_all(&gpu.queue, &mut encoder, time, beat, dt as f32, layers);
                break;
            }
        }

        // Build clip descriptors for compositor
        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());

        for clip in &tick_result.ready_clips {
            let texture_view = renderers.iter().find_map(|r| {
                r.as_any().downcast_ref::<GeneratorRenderer>()
                    .and_then(|gen| gen.get_clip_texture_view(&clip.id))
            });
            if let Some(view) = texture_view {
                let layer = layers.get(clip.layer_index as usize);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &clip.id,
                    texture_view: view,
                    layer_index: clip.layer_index,
                    blend_mode: layer.map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
                    translate_x: clip.translate_x,
                    translate_y: clip.translate_y,
                    scale: clip.scale,
                    rotation: clip.rotation,
                    invert_colors: clip.invert_colors,
                    effects: &clip.effects,
                    effect_groups: clip.effect_groups.as_deref().unwrap_or(&[]),
                });
            }
        }

        // Build layer descriptors for compositor
        let empty_effects: Vec<EffectInstance> = Vec::new();
        let empty_groups: Vec<EffectGroup> = Vec::new();
        let layer_descs: Vec<CompositeLayerDescriptor> = layers.iter().map(|layer| {
            CompositeLayerDescriptor {
                layer_index: layer.index,
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                effects: layer.effects.as_deref().unwrap_or(&empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(&empty_groups),
            }
        }).collect();

        // Composite
        let master_effects = project.map_or(&empty_effects[..], |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(&empty_groups);

        let frame = CompositorFrame {
            time,
            beat,
            dt: dt as f32,
            frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
            layers: &layer_descs,
            master_effects,
            master_effect_groups,
            tonemap: TonemapSettings::default(),
        };

        // Render compositor (records into encoder, returns view into tonemap output)
        let _compositor_view = self.compositor.render(&gpu.device, &gpu.queue, &mut encoder, &frame);

        // Copy compositor tonemap output → back buffer via texture copy
        let back_index = 1 - self.front_index;
        let bufs = self.output_buffers.as_ref().unwrap();
        let (comp_w, comp_h) = self.compositor.dimensions();
        let copy_size = wgpu::Extent3d {
            width: comp_w.min(bufs[back_index].width),
            height: comp_h.min(bufs[back_index].height),
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: self.compositor.output_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &bufs[back_index].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            copy_size,
        );

        // Submit all GPU work (generators + compositor + texture copy)
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // Swap: back becomes front
        self.front_index = back_index;

        // Update shared output view for the UI thread (zero copy — same device)
        let bufs = self.output_buffers.as_ref().unwrap();
        let front_view = bufs[self.front_index].texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.shared_output.set_view(front_view);
    }

    /// The stable output texture view. UI reads this for blitting.
    /// Returns None only before the first render.
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.output_buffers.as_ref().map(|bufs| &bufs[self.front_index].view)
    }

    /// Whether it's time for a content frame (for separate cadence mode).
    pub fn should_render_content(&self, realtime_now: f64) -> bool {
        realtime_now - self.last_content_time >= self.content_interval_secs
    }

    /// Mark that a content frame was rendered at the given time.
    pub fn mark_content_rendered(&mut self, realtime_now: f64) {
        self.last_content_time = realtime_now;
    }

    /// Set content rendering frame rate (independent of UI refresh rate).
    #[allow(dead_code)]
    pub fn set_content_fps(&mut self, fps: f64) {
        self.content_interval_secs = 1.0 / fps.max(1.0);
    }

    /// Resize compositor, generators, and output buffers to new project resolution.
    pub fn resize(&mut self, device: &wgpu::Device, engine: &mut PlaybackEngine, width: u32, height: u32) {
        self.compositor.resize(device, width, height);
        // Resize generator renderer via engine downcast
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                gen.resize_gpu(width, height);
                break;
            }
        }
        // Recreate output buffers at new dimensions
        if self.output_buffers.is_some() {
            self.output_buffers = Some([
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Front"),
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Back"),
            ]);
            self.front_index = 0;
        }
        // Update shared dimensions for UI thread
        self.shared_output.set_dimensions(width, height);
    }

    /// Get current compositor output dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        self.compositor.dimensions()
    }

    /// Pre-tonemap HDR output for export pipeline (GAP-IO-4).
    #[allow(dead_code)]
    pub fn pre_tonemap_output(&self) -> &wgpu::TextureView {
        self.compositor.pre_tonemap_output()
    }
}
