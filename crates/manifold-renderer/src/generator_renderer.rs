use std::any::Any;
use ahash::AHashMap;
use manifold_core::{Beats, GeneratorTypeId, LayerId, Seconds};
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_playback::renderer::ClipRenderer;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::generator::Generator;
use crate::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use crate::generators::registry::GeneratorRegistry;
use crate::uniform_arena::UniformArena;

/// Per-clip active state.
struct ActiveClip {
    /// Generator renders into this texture (may be reduced resolution).
    render_target: RenderTarget,
    /// Full-resolution output texture for upscaled generators.
    /// None when the generator runs at full resolution (scale = 1.0).
    upscale_target: Option<RenderTarget>,
    /// Internal resolution scale of this clip's generator (cached from trait).
    internal_scale: f32,
    generator_type: GeneratorTypeId,
    layer_id: LayerId,
    layer_index: i32, // positional cache for param lookup in render_all
    anim_progress: f32,
}

impl ActiveClip {
    /// The texture to hand to the compositor (upscaled if needed, else direct).
    fn output_texture(&self) -> &manifold_gpu::GpuTexture {
        self.upscale_target
            .as_ref()
            .map_or(&self.render_target.texture, |rt| &rt.texture)
    }
}

/// Per-layer generator state. Persists across clips to maintain
/// temporal state (particle positions, attractors, etc.).
struct LayerGeneratorState {
    generator: Box<dyn Generator>,
    generator_type: GeneratorTypeId,
    trigger_count: u32,
}

/// GPU-side clip renderer for generators.
/// Manages per-layer Generator instances and per-clip RenderTargets.
/// Port of C# GeneratorRenderer : IClipRenderer.
///
/// Generators with `internal_resolution_scale() < 1.0` render to reduced-resolution
/// render targets, then are upscaled to full output resolution via MetalFX Spatial
/// (or MPS Lanczos fallback). This matches Unity's `InternalResolutionScale` pattern
/// where organic/particle generators run at 0.5× and geometric generators at 1.0×.
pub struct GeneratorRenderer {
    /// Cached pointer to GpuDevice owned by ContentPipeline (same thread, same lifetime).
    device_ptr: *const GpuDevice,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    registry: GeneratorRegistry,
    active_clips: AHashMap<String, ActiveClip>,
    layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<String>,
    /// Per-clip render info: (layer_index, trigger_count, anim_progress, internal_scale).
    /// Parallel to render_scratch — avoids LayerId/GeneratorTypeId clones in render loop.
    render_info_scratch: Vec<(i32, u32, f32, f32)>,
    /// Shared-memory uniform arena for generator uniform data.
    /// Eliminates per-generator queue.write_buffer() calls.
    uniform_arena: UniformArena,
    /// Texture upscaler for reduced-resolution generators.
    /// Uses MetalFX Spatial when available, MPS Lanczos as fallback.
    upscaler: manifold_gpu::metalfx::TextureUpscaler,
    /// When false, all generators render at full resolution (Native mode).
    /// Controlled by `UpscaleMode` from project settings.
    scaling_enabled: bool,
}

// Safety: device_ptr points to GpuDevice on the content thread.
// GeneratorRenderer is only used on the content thread.
unsafe impl Send for GeneratorRenderer {}

impl GeneratorRenderer {
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
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

        let uniform_arena = UniformArena::new(device);
        let upscaler = manifold_gpu::metalfx::TextureUpscaler::new(device, format);

        let registry = GeneratorRegistry::new(format);
        // Pre-compile all generator pipelines into the binary archive.
        // Generators are created and immediately dropped — compiled Metal pipeline
        // binaries persist in the archive. Eliminates first-use stutter.
        registry.prewarm_all(device);

        Self {
            device_ptr: device as *const GpuDevice,
            width,
            height,
            format,
            registry,
            active_clips: AHashMap::with_capacity(16),
            layer_generators: AHashMap::with_capacity(8),
            available_rts,
            render_scratch: Vec::with_capacity(16),
            render_info_scratch: Vec::with_capacity(16),
            uniform_arena,
            upscaler,
            scaling_enabled: true,
        }
    }

    /// Set the device pointer after the GpuDevice has been moved to its
    /// final location (inside ContentPipeline). Must be called before any
    /// generator is created.
    pub fn set_device(&mut self, device: &GpuDevice) {
        self.device_ptr = device as *const GpuDevice;
    }

    /// Set whether internal resolution scaling is active.
    /// When disabled (Native mode), all generators render at full output resolution.
    /// When enabled, generators with `internal_resolution_scale() < 1.0` render at
    /// reduced resolution and are upscaled via MetalFX/MPS.
    pub fn set_scaling_enabled(&mut self, enabled: bool) {
        self.scaling_enabled = enabled;
    }

    /// Set the upscale method (MetalFX Spatial vs MPS Lanczos).
    pub fn set_upscale_mode(&mut self, mode: manifold_gpu::metalfx::UpscaleMode) {
        self.upscaler.set_mode(mode);
    }

    /// Get a reference to the GpuDevice.
    fn device(&self) -> &GpuDevice {
        unsafe { &*self.device_ptr }
    }

    /// Compute the reduced resolution for a generator with the given scale.
    /// Matches Unity's SetTrailResolution: clamp scale, round, minimum 16px.
    fn scaled_dimensions(width: u32, height: u32, scale: f32) -> (u32, u32) {
        let scale = scale.clamp(0.125, 1.0);
        let sw = (width as f32 * scale).round() as u32;
        let sh = (height as f32 * scale).round() as u32;
        (sw.max(16), sh.max(16))
    }

    /// Internal: acquire a clip with generator type and layer identity.
    /// Port of C# GeneratorRenderer.Acquire().
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: GeneratorTypeId,
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
            if let Some(generator) = self.registry.create(self.device(), &gen_type) {
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: gen_type.clone(),
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

        // Query generator's internal resolution scale (disabled in Native mode)
        let internal_scale = if self.scaling_enabled {
            self.layer_generators
                .get(&layer_id)
                .map_or(1.0, |ls| ls.generator.internal_resolution_scale())
        } else {
            1.0
        };
        let needs_upscale = internal_scale < 1.0;

        // Create render target at appropriate resolution
        let (rt_w, rt_h) = if needs_upscale {
            Self::scaled_dimensions(self.width, self.height, internal_scale)
        } else {
            (self.width, self.height)
        };

        let render_target = if !needs_upscale {
            // Full-res generator: try to reuse from pool
            if let Some(rt) = self.available_rts.pop() {
                rt
            } else {
                RenderTarget::new(
                    self.device(),
                    rt_w,
                    rt_h,
                    self.format,
                    "Generator RT (overflow)",
                )
            }
        } else {
            // Reduced-res generator: always create at scaled size
            log::debug!(
                "[GenRenderer] Clip {clip_id}: rendering at {}x{} ({:.0}% of {}x{}), upscale to full",
                rt_w, rt_h, internal_scale * 100.0, self.width, self.height,
            );
            RenderTarget::new(
                self.device(),
                rt_w,
                rt_h,
                self.format,
                &format!("Generator RT ({}x{} @ {:.0}%)", rt_w, rt_h, internal_scale * 100.0),
            )
        };

        // Create upscale target at full resolution if needed
        let upscale_target = if needs_upscale {
            let target = if let Some(rt) = self.available_rts.pop() {
                rt
            } else {
                RenderTarget::new(
                    self.device(),
                    self.width,
                    self.height,
                    self.format,
                    "Generator Upscale RT",
                )
            };
            Some(target)
        } else {
            None
        };

        self.active_clips.insert(
            clip_id.to_string(),
            ActiveClip {
                render_target,
                upscale_target,
                internal_scale,
                generator_type: gen_type.clone(),
                layer_id,
                layer_index,
                anim_progress: 0.0,
            },
        );

        true
    }

    /// Render all active generator clips.
    /// Called from app layer with full GPU context (encoder).
    /// Port of C# GeneratorRenderer.RenderAll().
    pub fn render_all(
        &mut self,
        gpu: &mut GpuEncoder,
        time: f64,
        beat: f64,
        dt: f32,
        layers: &[Layer],
    ) {
        // Reset uniform arena for this frame and set on GpuEncoder.
        self.uniform_arena.reset();
        gpu.uniform_arena =
            Some(&mut self.uniform_arena as *mut UniformArena);

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

        // Pre-collect (layer_index, trigger_count, anim_progress, internal_scale)
        // per clip during immutable borrow, avoiding per-clip LayerId/GeneratorTypeId clones.
        self.render_info_scratch.clear();
        for id in &self.render_scratch {
            if let Some(active) = self.active_clips.get(id.as_str()) {
                let trigger_count = self
                    .layer_generators
                    .get(&active.layer_id)
                    .map_or(0, |ls| ls.trigger_count);
                self.render_info_scratch.push((
                    active.layer_index,
                    trigger_count,
                    active.anim_progress,
                    active.internal_scale,
                ));
            } else {
                // Sentinel: skip this clip in the render loop
                self.render_info_scratch.push((-1, 0, 0.0, 1.0));
            }
        }

        for clip_idx in 0..self.render_scratch.len() {
            let id = &self.render_scratch[clip_idx];
            let (layer_index, trigger_count, anim_progress, internal_scale) =
                self.render_info_scratch[clip_idx];
            if layer_index < 0 {
                continue; // sentinel — clip not found
            }

            // Build GeneratorContext from layer params (zero allocation)
            let mut params = [0.0f32; MAX_GEN_PARAMS];
            let mut param_count = 0u32;
            if let Some(layer) = layers.get(layer_index as usize)
                && let Some(gp) = layer.gen_params()
            {
                param_count = gp.param_values.len().min(MAX_GEN_PARAMS) as u32;
                for (i, val) in gp.param_values.iter().take(MAX_GEN_PARAMS).enumerate() {
                    params[i] = *val;
                }
            }

            // For scaled generators, pass reduced dimensions in the context.
            // The generator sees the reduced resolution as its world, then we upscale.
            let (ctx_w, ctx_h) = if internal_scale < 1.0 {
                Self::scaled_dimensions(self.width, self.height, internal_scale)
            } else {
                (self.width, self.height)
            };

            let ctx = GeneratorContext {
                time,
                beat,
                dt,
                width: ctx_w,
                height: ctx_h,
                aspect: self.width as f32 / self.height as f32, // aspect stays at output ratio
                anim_progress,
                trigger_count,
                params,
                param_count,
            };

            // Split borrows: use layers[layer_index].layer_id (from the external
            // `layers` slice, not from `self`) for the layer_generators lookup.
            // This avoids cloning LayerId — layers[i].layer_id == active.layer_id
            // is guaranteed by the positional cache refresh above.
            if let Some(layer) = layers.get(layer_index as usize)
                && let Some(layer_state) = self.layer_generators.get_mut(&layer.layer_id)
                && let Some(active) = self.active_clips.get_mut(id.as_str())
            {
                let new_progress = layer_state.generator.render(
                    gpu,
                    &active.render_target.texture,
                    &ctx,
                );
                active.anim_progress = new_progress;
            }
        }

        // Flush uniform arena (recreates buffer if capacity grew).
        self.uniform_arena.flush(gpu.device);
        // Clear the arena pointer from GpuEncoder.
        gpu.uniform_arena = None;

        // ── Upscale pass: reduced-res generators → full-res output ───
        // Uses MetalFX Spatial when available, MPS Lanczos as fallback.
        // Must happen after all generators have rendered and arena is flushed.
        // Safety: device_ptr is valid for the lifetime of ContentPipeline.
        let device = unsafe { &*self.device_ptr };
        for id in &self.render_scratch {
            let active = match self.active_clips.get(id) {
                Some(a) => a,
                None => continue,
            };
            if let Some(ref upscale_rt) = active.upscale_target {
                // Get raw pointers to avoid borrow conflicts with self.upscaler
                let src_tex = &active.render_target.texture as *const manifold_gpu::GpuTexture;
                let dst_tex = &upscale_rt.texture as *const manifold_gpu::GpuTexture;
                // Safety: textures are valid for the duration of this frame.
                // No aliasing: src and dst are different textures, upscaler borrows
                // are disjoint from active_clips reads.
                self.upscaler.upscale(
                    gpu.native_enc,
                    device,
                    unsafe { &*src_tex },
                    unsafe { &*dst_tex },
                );
            }
        }
    }

    /// Get the animation progress for a rendered clip (for profiling).
    pub fn get_clip_anim_progress(&self, clip_id: &str) -> f32 {
        self.active_clips
            .get(clip_id)
            .map_or(0.0, |a| a.anim_progress)
    }

    /// Get the texture for a rendered clip (used by compositor).
    /// Returns the upscaled full-res texture for scaled generators,
    /// or the direct render target for full-res generators.
    pub fn get_clip_texture(&self, clip_id: &str) -> Option<&manifold_gpu::GpuTexture> {
        self.active_clips
            .get(clip_id)
            .map(|a| a.output_texture())
    }

    /// Resize all render targets and generators.
    pub fn resize_gpu(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        // Invalidate cached MetalFX scalers (dimension-specific).
        self.upscaler.invalidate();
        // Safety: device_ptr points to GpuDevice owned by ContentPipeline,
        // which outlives GeneratorRenderer. No aliasing with active_clips/generators.
        let device = unsafe { &*self.device_ptr };
        for active in self.active_clips.values_mut() {
            let (rt_w, rt_h) = if active.internal_scale < 1.0 {
                Self::scaled_dimensions(width, height, active.internal_scale)
            } else {
                (width, height)
            };
            active.render_target.resize(device, rt_w, rt_h);
            if let Some(ref mut upscale_rt) = active.upscale_target {
                upscale_rt.resize(device, width, height);
            }
        }
        for rt in &mut self.available_rts {
            rt.resize(device, width, height);
        }
        for layer_state in self.layer_generators.values_mut() {
            let scale = layer_state.generator.internal_resolution_scale();
            let (gen_w, gen_h) = if scale < 1.0 {
                Self::scaled_dimensions(width, height, scale)
            } else {
                (width, height)
            };
            layer_state.generator.resize(device, gen_w, gen_h);
        }
    }

    /// Reset all generator simulation state to initial conditions.
    /// Called after export warmup re-seek.
    pub fn reset_all_generator_state(&mut self) {
        let device = unsafe { &*self.device_ptr };
        for layer_state in self.layer_generators.values_mut() {
            layer_state.generator.reset_state(device);
        }
    }

    /// Update active clip types for a layer after generator type change.
    /// Port of C# GeneratorRenderer.UpdateActiveTypesForLayer().
    pub fn update_active_types_for_layer(
        &mut self,
        layer_id: &LayerId,
        new_type: GeneratorTypeId,
    ) {
        // Update clip type tracking
        for active in self.active_clips.values_mut() {
            if active.layer_id == *layer_id {
                active.generator_type = new_type.clone();
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
            if let Some(generator) = self.registry.create(self.device(), &new_type) {
                // Update internal_scale on active clips for this layer
                let new_scale = generator.internal_resolution_scale();
                // Safety: device_ptr is valid for the lifetime of ContentPipeline.
                let device = unsafe { &*self.device_ptr };
                let width = self.width;
                let height = self.height;
                let format = self.format;
                for active in self.active_clips.values_mut() {
                    if active.layer_id == *layer_id && active.internal_scale != new_scale {
                        active.internal_scale = new_scale;
                        if new_scale < 1.0 {
                            let (sw, sh) =
                                Self::scaled_dimensions(width, height, new_scale);
                            active.render_target.resize(device, sw, sh);
                            if active.upscale_target.is_none() {
                                active.upscale_target = Some(RenderTarget::new(
                                    device,
                                    width,
                                    height,
                                    format,
                                    "Generator Upscale RT",
                                ));
                            }
                        } else {
                            active.render_target.resize(device, width, height);
                            active.upscale_target = None;
                        }
                    }
                }

                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: new_type.clone(),
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

    fn start_clip(&mut self, clip: &TimelineClip, _current_time: Seconds, layers: &[Layer]) -> bool {
        let layer_id = clip.layer_id.clone();
        let layer_index = layers
            .iter()
            .position(|l| l.layer_id == layer_id)
            .map_or(0, |i| i as i32);
        self.acquire_clip(&clip.id, clip.generator_type.clone(), layer_id, layer_index)
    }

    fn stop_clip(&mut self, clip_id: &str) {
        if let Some(active) = self.active_clips.remove(clip_id) {
            // Return full-res RTs to pool (reduced-res RTs are dropped)
            if let Some(upscale_rt) = active.upscale_target {
                self.available_rts.push(upscale_rt);
            } else {
                self.available_rts.push(active.render_target);
            }

            // If no remaining active clips reference this layer, remove the
            // layer generator state to free GPU resources (particle buffers,
            // density textures, etc.).
            let layer_id = &active.layer_id;
            let has_remaining = self
                .active_clips
                .values()
                .any(|a| a.layer_id == *layer_id);
            if !has_remaining {
                self.layer_generators.remove(layer_id);
            }
        }
    }

    fn release_all(&mut self) {
        for (_, active) in self.active_clips.drain() {
            if let Some(upscale_rt) = active.upscale_target {
                self.available_rts.push(upscale_rt);
            } else {
                self.available_rts.push(active.render_target);
            }
        }
        // Release per-layer generator state (particle buffers, density textures, etc.)
        // to prevent GPU memory leaks across project switches.
        self.layer_generators.clear();
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

    fn needs_prepare_phase(&self) -> bool {
        false
    }
    fn needs_drift_correction(&self) -> bool {
        false
    }
    fn needs_pending_pause(&self) -> bool {
        false
    }

    fn get_clip_playback_time(&self, _clip_id: &str) -> f32 {
        0.0
    }
    fn get_clip_media_length(&self, _clip_id: &str) -> f32 {
        0.0
    }

    fn resume_clip(&mut self, _clip_id: &str) { /* no-op: generators render every frame */ }
    fn pause_clip(&mut self, _clip_id: &str) { /* no-op */ }
    fn seek_clip(&mut self, _clip_id: &str, _video_time: f32) { /* no-op */ }
    fn set_clip_looping(&mut self, _clip_id: &str, _looping: bool) { /* no-op */ }
    fn set_clip_playback_rate(&mut self, _clip_id: &str, _rate: f32) { /* no-op */ }

    fn pre_render(&mut self, _time: Seconds, _beat: Beats, _dt: f32) {
        // No-op: actual GPU rendering is done via render_all() called from app
        // with encoder context that the trait can't provide.
        // Unity's PreRender delegates to RenderAll, but Rust needs explicit GPU context.
    }

    fn resize(&mut self, width: i32, height: i32) {
        self.resize_gpu(width as u32, height as u32);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
