#![allow(dead_code)]
use std::sync::{Arc, RwLock};

use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
#[cfg(not(target_os = "macos"))]
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

/// Output format for double-buffered compositor output (non-macOS fallback).
/// Matches compositor's tonemap output format.
#[cfg(not(target_os = "macos"))]
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
    /// NOT used on macOS (IOSurface path bypasses double-buffering).
    #[cfg(not(target_os = "macos"))]
    output_buffers: Option<[RenderTarget; 2]>,
    /// Which buffer is the front (0 or 1). Back is always `1 - front_index`.
    #[cfg(not(target_os = "macos"))]
    front_index: usize,
    /// Content frame rate tracking (for separate cadence mode).
    content_interval_secs: f64,
    last_content_time: f64,
    /// Shared output view for cross-thread access (fallback for non-macOS).
    shared_output: Arc<SharedOutputView>,
    /// IOSurface-backed texture on the content device. Compositor output is
    /// copied here; the UI device reads the same GPU memory via its own texture.
    #[cfg(target_os = "macos")]
    shared_texture: Option<wgpu::Texture>,
    /// IOSurface bridge for cross-device sharing.
    #[cfg(target_os = "macos")]
    shared_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen bridge generation — used to detect resize and re-import.
    #[cfg(target_os = "macos")]
    shared_generation: u64,
}

impl ContentPipeline {
    pub fn new(compositor: Box<dyn Compositor>) -> Self {
        let shared = Arc::new(SharedOutputView::new());
        Self {
            compositor,
            #[cfg(not(target_os = "macos"))]
            output_buffers: None,
            #[cfg(not(target_os = "macos"))]
            front_index: 0,
            content_interval_secs: 1.0 / 60.0,
            last_content_time: 0.0,
            shared_output: shared,
            #[cfg(target_os = "macos")]
            shared_texture: None,
            #[cfg(target_os = "macos")]
            shared_bridge: None,
            #[cfg(target_os = "macos")]
            shared_generation: 0,
        }
    }

    /// Set the IOSurface shared texture and bridge. Called during init after
    /// the bridge imports a texture on the content device.
    #[cfg(target_os = "macos")]
    pub fn set_shared_texture(
        &mut self,
        texture: wgpu::Texture,
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.shared_texture = Some(texture);
        self.shared_bridge = Some(bridge);
    }

    /// Get a clone of the shared output handle. The UI thread holds this
    /// to read the front buffer view and dimensions.
    pub fn shared_output(&self) -> Arc<SharedOutputView> {
        Arc::clone(&self.shared_output)
    }

    /// Lazily create the double-buffer pair at compositor dimensions.
    /// Only used on non-macOS (macOS uses IOSurface zero-copy path).
    #[cfg(not(target_os = "macos"))]
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
        #[cfg(not(target_os = "macos"))]
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
        // Use static empty slices instead of per-frame Vec allocations.
        let empty_effects: &[EffectInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];
        let layer_descs: Vec<CompositeLayerDescriptor> = layers.iter().map(|layer| {
            CompositeLayerDescriptor {
                layer_index: layer.index,
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                effects: layer.effects.as_deref().unwrap_or(empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(empty_groups),
            }
        }).collect();

        // Composite
        let master_effects = project.map_or(empty_effects, |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(empty_groups);

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

        let (comp_w, comp_h) = self.compositor.dimensions();

        // Copy compositor output to the appropriate destination.
        // macOS: IOSurface shared texture (UI reads via its own imported texture).
        // Other: double-buffered output textures (UI reads via SharedOutputView).
        #[cfg(target_os = "macos")]
        {
            if let Some(ref shared_tex) = self.shared_texture {
                if shared_tex.width() == comp_w && shared_tex.height() == comp_h {
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: self.compositor.output_texture(),
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: shared_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: comp_w,
                            height: comp_h,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let back_index = 1 - self.front_index;
            let bufs = self.output_buffers.as_ref().unwrap();
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
        }

        // Submit all GPU work (generators + compositor + copy)
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // Wait for GPU to finish this frame before returning. Prevents command
        // buffer pileup that causes periodic stalls (3 frames queue, 4th blocks hard).
        // Makes frame timing consistent: GPU-bound frames run at steady reduced FPS
        // instead of 60-60-60-stall judder. Only blocks the content device — UI is
        // on its own device/queue and is completely unaffected.
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

        // Swap + update shared output view (non-macOS fallback path)
        #[cfg(not(target_os = "macos"))]
        {
            let back_index = 1 - self.front_index;
            self.front_index = back_index;
            let bufs = self.output_buffers.as_ref().unwrap();
            let front_view = bufs[self.front_index].texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.shared_output.set_view(front_view);
        }

        // Update shared dimensions for UI aspect ratio (only when changed).
        let (old_w, old_h) = self.shared_output.get_dimensions();
        if old_w != comp_w || old_h != comp_h {
            self.shared_output.set_dimensions(comp_w, comp_h);
        }
    }

    /// The stable output texture view. UI reads this for blitting.
    /// Returns None only before the first render.
    /// Only used on non-macOS (macOS reads via IOSurface).
    #[cfg(not(target_os = "macos"))]
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
        // Recreate output buffers at new dimensions (non-macOS only)
        #[cfg(not(target_os = "macos"))]
        if self.output_buffers.is_some() {
            self.output_buffers = Some([
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Front"),
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Back"),
            ]);
            self.front_index = 0;
        }
        // Resize IOSurface bridge and re-import content texture
        #[cfg(target_os = "macos")]
        if let Some(ref bridge) = self.shared_bridge {
            bridge.resize(width, height);
            self.shared_texture = Some(unsafe { bridge.import_texture(device) });
            self.shared_generation = bridge.generation();
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
