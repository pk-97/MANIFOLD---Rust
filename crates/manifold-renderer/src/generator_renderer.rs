use std::collections::HashMap;
use manifold_core::GeneratorType;
use manifold_core::layer::Layer;
use crate::render_target::RenderTarget;
use crate::generator::Generator;
use crate::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use crate::generators::registry::GeneratorRegistry;

/// Per-clip active state.
struct ActiveClip {
    render_target: RenderTarget,
    generator_type: GeneratorType,
    layer_index: i32,
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
pub struct GeneratorRenderer {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    registry: GeneratorRegistry,
    active_clips: HashMap<String, ActiveClip>,
    layer_generators: HashMap<i32, LayerGeneratorState>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<String>,
}

impl GeneratorRenderer {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        pool_size: usize,
    ) -> Self {
        let mut available_rts = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            available_rts.push(RenderTarget::new(
                device,
                width,
                height,
                format,
                &format!("Generator RT {i}"),
            ));
        }

        Self {
            width,
            height,
            format,
            registry: GeneratorRegistry::new(format),
            active_clips: HashMap::with_capacity(16),
            layer_generators: HashMap::with_capacity(8),
            available_rts,
            render_scratch: Vec::with_capacity(16),
        }
    }

    /// Start a generator clip. Returns true if successfully started.
    pub fn start_clip(
        &mut self,
        device: &wgpu::Device,
        clip_id: &str,
        gen_type: GeneratorType,
        layer_index: i32,
    ) -> bool {
        if self.active_clips.contains_key(clip_id) {
            return true;
        }

        // Ensure layer has a generator of the right type
        let needs_create = self
            .layer_generators
            .get(&layer_index)
            .is_none_or(|ls| ls.generator_type != gen_type);

        if needs_create {
            if let Some(gen) = self.registry.create(device, gen_type) {
                self.layer_generators.insert(
                    layer_index,
                    LayerGeneratorState {
                        generator: gen,
                        generator_type: gen_type,
                        trigger_count: 0,
                    },
                );
            } else {
                return false;
            }
        }

        if let Some(ls) = self.layer_generators.get_mut(&layer_index) {
            ls.trigger_count += 1;
        }

        // Acquire RT from pool or create new
        let rt = if let Some(rt) = self.available_rts.pop() {
            rt
        } else {
            RenderTarget::new(
                device,
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
                layer_index,
                anim_progress: 0.0,
            },
        );

        true
    }

    /// Stop a generator clip, returning its RT to the pool.
    pub fn stop_clip(&mut self, clip_id: &str) {
        if let Some(active) = self.active_clips.remove(clip_id) {
            self.available_rts.push(active.render_target);
        }
    }

    /// Render all active generator clips.
    #[allow(clippy::too_many_arguments)]
    pub fn render_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        time: f32,
        beat: f32,
        dt: f32,
        layers: &[Layer],
    ) {
        // Collect clip IDs into pre-allocated scratch to avoid borrow conflict
        self.render_scratch.clear();
        self.render_scratch
            .extend(self.active_clips.keys().cloned());

        for clip_id in 0..self.render_scratch.len() {
            let id = &self.render_scratch[clip_id];

            let (layer_index, gen_type, anim_progress) = {
                let active = match self.active_clips.get(id) {
                    Some(a) => a,
                    None => continue,
                };
                (active.layer_index, active.generator_type, active.anim_progress)
            };

            // Build GeneratorContext from layer params (zero allocation)
            let mut params = [0.0f32; MAX_GEN_PARAMS];
            let mut param_count = 0u32;
            if let Some(layer) = layers.get(layer_index as usize) {
                if let Some(gp) = &layer.gen_params {
                    param_count = gp.param_values.len().min(MAX_GEN_PARAMS) as u32;
                    for (i, val) in gp.param_values.iter().take(MAX_GEN_PARAMS).enumerate() {
                        params[i] = *val;
                    }
                }
            }

            let trigger_count = self
                .layer_generators
                .get(&layer_index)
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
            if let Some(layer_state) = self.layer_generators.get_mut(&layer_index) {
                if let Some(active) = self.active_clips.get_mut(id) {
                    let new_progress = layer_state.generator.render(
                        device,
                        queue,
                        encoder,
                        &active.render_target.view,
                        &ctx,
                    );
                    active.anim_progress = new_progress;
                }
            }
        }
    }

    /// Get the texture view for a rendered clip (used by compositor).
    pub fn get_clip_texture_view(&self, clip_id: &str) -> Option<&wgpu::TextureView> {
        self.active_clips.get(clip_id).map(|a| &a.render_target.view)
    }

    /// Check if a clip is active.
    pub fn is_active(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    /// Resize all render targets and generators.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for active in self.active_clips.values_mut() {
            active.render_target.resize(device, width, height);
        }
        for rt in &mut self.available_rts {
            rt.resize(device, width, height);
        }
        for layer_state in self.layer_generators.values_mut() {
            layer_state.generator.resize(device, width, height);
        }
    }

    /// Release all active clips, returning RTs to pool.
    pub fn release_all(&mut self) {
        for (_, active) in self.active_clips.drain() {
            self.available_rts.push(active.render_target);
        }
    }

    /// Number of active clips.
    pub fn active_count(&self) -> usize {
        self.active_clips.len()
    }
}
