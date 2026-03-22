use std::any::Any;
use ahash::AHashMap;
use std::sync::Arc;
use manifold_core::{GeneratorType, LayerId};
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_playback::renderer::ClipRenderer;
use crate::render_target::RenderTarget;
use crate::generator::Generator;
use crate::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use crate::generators::registry::GeneratorRegistry;

/// Per-clip active state.
struct ActiveClip {
    render_target: RenderTarget,
    generator_type: GeneratorType,
    layer_id: LayerId,
    layer_index: i32, // positional cache for param lookup in render_all
    anim_progress: f32,
}

/// Per-layer generator state. Persists across clips to maintain
/// temporal state (particle positions, attractors, etc.).
struct LayerGeneratorState {
    generator: Box<dyn Generator>,
    generator_type: GeneratorType,
    trigger_count: u32,
}

/// GPU-side clip renderer for generators.
/// Manages per-layer Generator instances and per-clip RenderTargets.
/// Port of C# GeneratorRenderer : IClipRenderer.
pub struct GeneratorRenderer {
    device: Arc<wgpu::Device>,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    registry: GeneratorRegistry,
    active_clips: AHashMap<String, ActiveClip>,
    layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<String>,
}

impl GeneratorRenderer {
    pub fn new(
        device: Arc<wgpu::Device>,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        pool_size: usize,
    ) -> Self {
        let mut available_rts = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            available_rts.push(RenderTarget::new(
                &device,
                width,
                height,
                format,
                &format!("Generator RT {i}"),
            ));
        }

        Self {
            device,
            width,
            height,
            format,
            registry: GeneratorRegistry::new(format),
            active_clips: AHashMap::with_capacity(16),
            layer_generators: AHashMap::with_capacity(8),
            available_rts,
            render_scratch: Vec::with_capacity(16),
        }
    }

    /// Internal: acquire a clip with generator type and layer identity.
    /// Port of C# GeneratorRenderer.Acquire().
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: GeneratorType,
        layer_id: LayerId,
        layer_index: i32,
    ) -> bool {
        if self.active_clips.contains_key(clip_id) {
            return true;
        }

        // Ensure layer has a generator of the right type
        let needs_create = self
            .layer_generators
            .get(&layer_id)
            .is_none_or(|ls| ls.generator_type != gen_type);

        if needs_create {
            if let Some(generator) = self.registry.create(&self.device, gen_type) {
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: gen_type,
                        trigger_count: 0,
                    },
                );
            } else {
                return false;
            }
        }

        if let Some(ls) = self.layer_generators.get_mut(&layer_id) {
            ls.trigger_count += 1;
        }

        // Acquire RT from pool or create new
        let rt = if let Some(rt) = self.available_rts.pop() {
            rt
        } else {
            RenderTarget::new(
                &self.device,
                self.width,
                self.height,
                self.format,
                "Generator RT (overflow)",
            )
        };

        self.active_clips.insert(
            clip_id.to_string(),
            ActiveClip {
                render_target: rt,
                generator_type: gen_type,
                layer_id,
                layer_index,
                anim_progress: 0.0,
            },
        );

        true
    }

    /// Render all active generator clips.
    /// Called from app layer with full GPU context (queue + encoder).
    /// Port of C# GeneratorRenderer.RenderAll().
    pub fn render_all(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        time: f32,
        beat: f32,
        dt: f32,
        layers: &[Layer],
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // Refresh positional cache on active clips — layer_index may have changed
        // after reorder, but layer_id stays stable so generator state follows.
        for active in self.active_clips.values_mut() {
            if let Some(pos) = layers.iter().position(|l| l.layer_id == active.layer_id) {
                active.layer_index = pos as i32;
            }
        }

        // Collect clip IDs into pre-allocated scratch to avoid borrow conflict
        self.render_scratch.clear();
        self.render_scratch
            .extend(self.active_clips.keys().cloned());

        for clip_id in 0..self.render_scratch.len() {
            let id = &self.render_scratch[clip_id];

            let (layer_id, layer_index, gen_type, anim_progress) = {
                let active = match self.active_clips.get(id) {
                    Some(a) => a,
                    None => continue,
                };
                (active.layer_id.clone(), active.layer_index, active.generator_type, active.anim_progress)
            };

            // Build GeneratorContext from layer params (zero allocation)
            let mut params = [0.0f32; MAX_GEN_PARAMS];
            let mut param_count = 0u32;
            if let Some(layer) = layers.get(layer_index as usize)
                && let Some(gp) = &layer.gen_params {
                    param_count = gp.param_values.len().min(MAX_GEN_PARAMS) as u32;
                    for (i, val) in gp.param_values.iter().take(MAX_GEN_PARAMS).enumerate() {
                        params[i] = *val;
                    }
                }

            let trigger_count = self
                .layer_generators
                .get(&layer_id)
                .map_or(0, |ls| ls.trigger_count);

            let ctx = GeneratorContext {
                time,
                beat,
                dt,
                width: self.width,
                height: self.height,
                aspect: self.width as f32 / self.height as f32,
                anim_progress,
                trigger_count,
                params,
                param_count,
            };

            // Split borrows: get generator and active clip's RT view separately
            let _ = gen_type; // used for type matching if needed
            if !self.layer_generators.contains_key(&layer_id) {
                log::error!("[GenRenderer] clip {} layer_id={} — NO generator found! map has {:?}",
                    id, layer_id, self.layer_generators.keys().collect::<Vec<_>>());
            }
            if let Some(layer_state) = self.layer_generators.get_mut(&layer_id)
                && let Some(active) = self.active_clips.get_mut(id) {
                    if let Some(profiler) = gpu_profiler {
                        profiler.set_scope(&format!("clip:{}:", id));
                    }
                    let new_progress = layer_state.generator.render(
                        &self.device,
                        queue,
                        encoder,
                        &active.render_target.view,
                        &ctx,
                        gpu_profiler,
                    );
                    if let Some(profiler) = gpu_profiler {
                        profiler.clear_scope();
                    }
                    active.anim_progress = new_progress;
                }
        }
    }

    /// Get the animation progress for a rendered clip (for profiling).
    pub fn get_clip_anim_progress(&self, clip_id: &str) -> f32 {
        self.active_clips.get(clip_id).map_or(0.0, |a| a.anim_progress)
    }

    /// Get the texture view for a rendered clip (used by compositor).
    pub fn get_clip_texture_view(&self, clip_id: &str) -> Option<&wgpu::TextureView> {
        self.active_clips.get(clip_id).map(|a| &a.render_target.view)
    }

    /// Resize all render targets and generators.
    pub fn resize_gpu(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for active in self.active_clips.values_mut() {
            active.render_target.resize(&self.device, width, height);
        }
        for rt in &mut self.available_rts {
            rt.resize(&self.device, width, height);
        }
        for layer_state in self.layer_generators.values_mut() {
            layer_state.generator.resize(&self.device, width, height);
        }
    }

    /// Update active clip types for a layer after generator type change.
    /// Port of C# GeneratorRenderer.UpdateActiveTypesForLayer().
    pub fn update_active_types_for_layer(&mut self, layer_id: &LayerId, new_type: GeneratorType) {
        // Update clip type tracking
        for active in self.active_clips.values_mut() {
            if active.layer_id == *layer_id {
                active.generator_type = new_type;
            }
        }

        // If the type changed, force the generator swap now.
        let needs_swap = self
            .layer_generators
            .get(layer_id)
            .is_some_and(|ls| ls.generator_type != new_type);

        if needs_swap {
            let old_trigger_count = self
                .layer_generators
                .get(layer_id)
                .map_or(0, |ls| ls.trigger_count);
            if let Some(generator) = self.registry.create(&self.device, new_type) {
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: new_type,
                        trigger_count: old_trigger_count,
                    },
                );
            }
        }
    }

    /// Number of active clips.
    pub fn active_count(&self) -> usize {
        self.active_clips.len()
    }
}

// =====================================================================
// IClipRenderer implementation
// Port of C# GeneratorRenderer : IClipRenderer
// =====================================================================

impl ClipRenderer for GeneratorRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        clip.is_generator()
    }

    fn start_clip(&mut self, clip: &TimelineClip, _current_time: f32, layers: &[Layer]) -> bool {
        let layer_id = layers.get(clip.layer_index as usize)
            .map(|l| l.layer_id.clone())
            .unwrap_or_default();
        self.acquire_clip(&clip.id, clip.generator_type, layer_id, clip.layer_index)
    }

    fn stop_clip(&mut self, clip_id: &str) {
        if let Some(active) = self.active_clips.remove(clip_id) {
            self.available_rts.push(active.render_target);
        }
    }

    fn release_all(&mut self) {
        for (_, active) in self.active_clips.drain() {
            self.available_rts.push(active.render_target);
        }
    }

    fn is_clip_ready(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    fn is_active(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    fn is_clip_playing(&self, clip_id: &str) -> bool {
        // Unity: IsClipPlaying => IsActive (generators always "playing")
        self.active_clips.contains_key(clip_id)
    }

    fn needs_prepare_phase(&self) -> bool { false }
    fn needs_drift_correction(&self) -> bool { false }
    fn needs_pending_pause(&self) -> bool { false }

    fn get_clip_playback_time(&self, _clip_id: &str) -> f32 { 0.0 }
    fn get_clip_media_length(&self, _clip_id: &str) -> f32 { 0.0 }

    fn resume_clip(&mut self, _clip_id: &str) { /* no-op: generators render every frame */ }
    fn pause_clip(&mut self, _clip_id: &str) { /* no-op */ }
    fn seek_clip(&mut self, _clip_id: &str, _video_time: f32) { /* no-op */ }
    fn set_clip_looping(&mut self, _clip_id: &str, _looping: bool) { /* no-op */ }
    fn set_clip_playback_rate(&mut self, _clip_id: &str, _rate: f32) { /* no-op */ }

    fn pre_render(&mut self, _time: f32, _beat: f32, _dt: f32) {
        // No-op: actual GPU rendering is done via render_all() called from app
        // with queue/encoder context that the trait can't provide.
        // Unity's PreRender delegates to RenderAll, but Rust needs explicit GPU context.
    }

    fn resize(&mut self, width: i32, height: i32) {
        self.resize_gpu(width as u32, height as u32);
    }

    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
