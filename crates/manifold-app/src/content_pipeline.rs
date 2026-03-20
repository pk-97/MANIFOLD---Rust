use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
use manifold_renderer::tonemap::TonemapSettings;
use manifold_playback::engine::{PlaybackEngine, TickResult};

/// Self-contained content rendering pipeline.
///
/// Owns the compositor and orchestrates GPU rendering of generators + compositing.
/// The PlaybackEngine (which owns GeneratorRenderer) is borrowed for each frame.
///
/// This is the unit that will eventually move to its own thread for
/// independent content frame rate (decoupled from UI refresh rate).
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
}

impl ContentPipeline {
    pub fn new(compositor: Box<dyn Compositor>) -> Self {
        Self { compositor }
    }

    /// Render all generators and composite into the final output texture.
    ///
    /// Returns the tonemapped output texture view (lifetime tied to `&self`).
    /// The caller must store this as a raw pointer before calling present,
    /// since present needs `&mut Application`.
    pub fn render_content(
        &mut self,
        gpu: &GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
    ) -> &wgpu::TextureView {
        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();

        let mut encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame Encoder"),
                });

        // Split borrow: get renderers + project from engine simultaneously.
        // Engine now owns the real GeneratorRenderer (replaced stub in init_gpu),
        // so clip lifecycle (start/stop) is handled by engine's sync_clips_to_time.
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

        let output_view = self.compositor.render(&gpu.device, &gpu.queue, &mut encoder, &frame);

        // Submit generator + compositor work
        let output_view_ptr: *const wgpu::TextureView = output_view;
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // SAFETY: output_view points into self.compositor's RenderTarget which
        // is not modified between here and the blit calls in present_all_windows.
        unsafe { &*output_view_ptr }
    }

    /// Resize compositor and generators to new project resolution.
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
