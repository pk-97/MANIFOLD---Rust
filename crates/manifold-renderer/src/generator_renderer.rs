use crate::generator::Generator;
use crate::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use crate::generators::registry::GeneratorRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::uniform_arena::UniformArena;
use ahash::AHashMap;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::{Beats, ClipId, GeneratorTypeId, LayerId, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_playback::renderer::ClipRenderer;
use std::any::Any;

/// Per-clip active state.
struct ActiveClip {
    /// Generator renders into this texture at full output resolution.
    render_target: RenderTarget,
    generator_type: GeneratorTypeId,
    layer_id: LayerId,
    layer_index: i32, // positional cache for param lookup in render_all
    clip_index: u32,  // positional cache for string_params lookup (avoids linear scan)
    anim_progress: f32,
    /// True on the first frame after acquiring a reused render target.
    /// Cleared to opaque black before the generator renders to prevent
    /// stale content from a previous clip/layer leaking through.
    needs_clear: bool,
}

impl ActiveClip {
    /// The texture to hand to the compositor.
    fn output_texture(&self) -> &manifold_gpu::GpuTexture {
        &self.render_target.texture
    }
}

/// Per-layer generator state. Persists across clips to maintain
/// temporal state (particle positions, attractors, etc.).
struct LayerGeneratorState {
    generator: Box<dyn Generator>,
    generator_type: GeneratorTypeId,
    trigger_count: u32,
    /// `Layer::generator_graph_version` at the time this generator
    /// was constructed. When the layer's version bumps (graph-editor
    /// edit landed via `ToggleNodeParamExposeCommand` / `AddGraphNodeCommand`
    /// / `SetGraphNodeParamCommand` / etc), `acquire_clip` rebuilds
    /// the generator with the new override def. `None` when the
    /// generator was built from the bundled preset (no override
    /// present at construction time).
    override_version: Option<u32>,
    /// Cached string params from the layer's clips. When a clip provides a
    /// string param (e.g. fontFamily), it's stored here so that subsequent clips
    /// without that key still get the layer's value. This avoids the first-clip
    /// fallback-to-default problem where e.g. text renders in Inter before the
    /// clip with the selected font is reached.
    layer_string_defaults: std::collections::BTreeMap<String, String>,
    /// Cached merged string params (defaults + clip overrides). Rebuilt only
    /// when `string_params_dirty` is set (clip start, type change, data_version).
    merged_string_params: std::collections::BTreeMap<String, String>,
    /// True when merged_string_params needs to be rebuilt.
    string_params_dirty: bool,
}

/// GPU-side clip renderer for generators.
/// Manages per-layer Generator instances and per-clip RenderTargets.
///
/// All generators render at full output resolution. If a specific generator
/// needs internal downscaling for performance (e.g. raymarching, fluid sim),
/// it does so inside its own `render()` by allocating and managing its own
/// reduced-resolution intermediate textures — the runtime doesn't model it.
pub struct GeneratorRenderer {
    /// Cached pointer to GpuDevice owned by ContentPipeline (same thread, same lifetime).
    device_ptr: *const GpuDevice,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    registry: GeneratorRegistry,
    active_clips: AHashMap<ClipId, ActiveClip>,
    layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<ClipId>,
    /// Per-clip render info: (layer_index, clip_index, trigger_count, anim_progress).
    /// Parallel to render_scratch — avoids LayerId/GeneratorTypeId clones in render loop.
    render_info_scratch: Vec<(i32, u32, u32, f32)>,
    /// Shared-memory uniform arena for generator uniform data.
    /// Eliminates per-generator queue.write_buffer() calls.
    uniform_arena: UniformArena,
    /// Cached data_version — layer_index refresh scan only runs when this changes.
    last_data_version: u64,
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
        _pool_size: usize,
    ) -> Self {
        // Lazy allocation: start empty, grow on demand as clips start.
        // Avoids pre-allocating large textures that may never be used.
        let available_rts = Vec::with_capacity(8);

        let uniform_arena = UniformArena::new(device);

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
            last_data_version: u64::MAX, // force scan on first frame
        }
    }

    /// Set the device pointer after the GpuDevice has been moved to its
    /// final location (inside ContentPipeline). Must be called before any
    /// generator is created.
    pub fn set_device(&mut self, device: &GpuDevice) {
        self.device_ptr = device as *const GpuDevice;
    }

    /// Get a reference to the GpuDevice.
    fn device(&self) -> &GpuDevice {
        unsafe { &*self.device_ptr }
    }

    /// Internal: acquire a clip with generator type and layer identity.
    /// Port of C# GeneratorRenderer.Acquire().
    ///
    /// `override_def` is the layer's `generator_graph` field (the
    /// per-layer JSON-graph override the graph editor writes to);
    /// `override_version` is the matching `generator_graph_version`
    /// monotonic counter. When either changes — type swap, override
    /// added, version bump — the existing generator is dropped and
    /// rebuilt against the new state. `override_def = None` falls
    /// back to the bundled JSON preset.
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: GeneratorTypeId,
        layer_id: LayerId,
        layer_index: i32,
        clip_index: u32,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: u32,
    ) -> bool {
        if self.active_clips.contains_key(clip_id) {
            return true;
        }

        // Compare against the layer's current generator state. Rebuild
        // when: (a) no generator yet, (b) type changed, or (c) the
        // override version differs from what we last built against
        // (graph-editor edit landed). `None` here means "no override
        // present this frame"; `Some(v)` means "override is at v".
        // Encoding "no override" as `None` lets us distinguish the
        // initial bundled-preset path from a v0 override.
        let current_override_version: Option<u32> =
            override_def.map(|_| override_version);
        let needs_create = self
            .layer_generators
            .get(&layer_id)
            .is_none_or(|ls| {
                ls.generator_type != gen_type
                    || ls.override_version != current_override_version
            });

        if needs_create {
            // Preserve `trigger_count` across rebuild. Without this,
            // editing the override graph mid-clip OR changing the
            // generator type resets the counter to 0, which makes the
            // next clip-trigger's `count % N` calculation potentially
            // collide with the value the previous instance just
            // emitted — the user sees the same pattern back-to-back
            // even though the math should never produce duplicates.
            // The counter is conceptually "how many times has this
            // layer been triggered" and is generator-agnostic, so
            // carrying it forward is semantically correct.
            let preserved_trigger_count = self
                .layer_generators
                .get(&layer_id)
                .map(|ls| ls.trigger_count)
                .unwrap_or(0);
            if let Some(generator) =
                self.registry
                    .create_with_override(self.device(), &gen_type, override_def)
            {
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: gen_type.clone(),
                        trigger_count: preserved_trigger_count,
                        override_version: current_override_version,
                        layer_string_defaults: std::collections::BTreeMap::new(),
                        merged_string_params: std::collections::BTreeMap::new(),
                        string_params_dirty: true,
                    },
                );
            } else {
                return false;
            }
        }

        if let Some(ls) = self.layer_generators.get_mut(&layer_id) {
            ls.trigger_count += 1;
        }

        // Create render target at full output resolution. Pool-recycle when
        // possible — reused RTs may contain stale content from a different
        // clip/layer, so we clear before the generator renders.
        let render_target = if let Some(rt) = self.available_rts.pop() {
            rt
        } else {
            RenderTarget::new(
                self.device(),
                self.width,
                self.height,
                self.format,
                "Generator RT (overflow)",
            )
        };

        self.active_clips.insert(
            ClipId::new(clip_id),
            ActiveClip {
                render_target,
                generator_type: gen_type.clone(),
                layer_id,
                layer_index,
                clip_index,
                anim_progress: 0.0,
                needs_clear: true,
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
        data_version: u64,
    ) {
        // Reset uniform arena for this frame and set on GpuEncoder.
        self.uniform_arena.reset();
        gpu.uniform_arena = Some(&mut self.uniform_arena as *mut UniformArena);

        // Refresh positional cache on active clips — only when the project has
        // structurally changed (layer reorder/add/delete bumps data_version).
        // layer_id stays stable across reorders so generator state follows.
        if data_version != self.last_data_version {
            self.last_data_version = data_version;
            for (clip_id, active) in self.active_clips.iter_mut() {
                if let Some(pos) = layers.iter().position(|l| l.layer_id == active.layer_id) {
                    active.layer_index = pos as i32;
                    // Refresh clip_index within the layer (clips may reorder on edit).
                    active.clip_index = layers[pos]
                        .clips
                        .iter()
                        .position(|c| c.id == *clip_id)
                        .unwrap_or(0) as u32;
                }
            }
            // Structural change — string params may have changed, mark all dirty.
            for layer_state in self.layer_generators.values_mut() {
                layer_state.string_params_dirty = true;
            }
            // Event-based per-layer eviction: any `layer_generators`
            // entry whose LayerId is no longer present in the project
            // gets dropped, freeing its GPU resources (particle
            // buffers, fluid-sim density grids, attractor history
            // textures — each can be tens of MB). Mirrors the
            // compositor's `trim_excess_buffers` pattern but runs
            // only on data_version change (structural edit), not per
            // frame.
            let alive: ahash::AHashSet<&manifold_core::LayerId> =
                layers.iter().map(|l| &l.layer_id).collect();
            self.layer_generators.retain(|id, _| alive.contains(id));
        }

        // Per-frame override-version sweep. `acquire_clip` only
        // rebuilds the generator on clip start, so without this pass
        // a graph-editor edit on an already-active layer wouldn't
        // pick up the new bindings until the clip restarts.
        // Iterate over a snapshot of active layer ids and compare
        // each layer's `generator_graph_version` against the version
        // captured in `LayerGeneratorState.override_version`; rebuild
        // on mismatch, preserving `trigger_count`.
        //
        // Allocates a tiny `Vec<LayerId>` per frame (one entry per
        // active layer, typically 1-5). Acceptable for this rebuild
        // path since the alternative would require restructuring
        // `layer_generators` for split borrows.
        let layer_ids_to_check: Vec<manifold_core::LayerId> = self
            .layer_generators
            .keys()
            .cloned()
            .collect();
        for layer_id in &layer_ids_to_check {
            let Some(layer) = layers.iter().find(|l| &l.layer_id == layer_id) else {
                continue;
            };
            let current_override_version: Option<u32> =
                layer.generator_graph.as_ref().map(|_| layer.generator_graph_version);
            let needs_rebuild = self
                .layer_generators
                .get(layer_id)
                .is_some_and(|ls| ls.override_version != current_override_version);
            if !needs_rebuild {
                continue;
            }
            let preserved_trigger_count = self
                .layer_generators
                .get(layer_id)
                .map(|ls| ls.trigger_count)
                .unwrap_or(0);
            let gen_type = layer.generator_type().clone();
            let override_def = layer.generator_graph.as_ref();
            if let Some(generator) =
                self.registry
                    .create_with_override(self.device(), &gen_type, override_def)
            {
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: gen_type,
                        trigger_count: preserved_trigger_count,
                        override_version: current_override_version,
                        layer_string_defaults: std::collections::BTreeMap::new(),
                        merged_string_params: std::collections::BTreeMap::new(),
                        string_params_dirty: true,
                    },
                );
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
                    active.clip_index,
                    trigger_count,
                    active.anim_progress,
                ));
            } else {
                // Sentinel: skip this clip in the render loop
                self.render_info_scratch.push((-1, 0, 0, 0.0));
            }
        }

        for clip_idx in 0..self.render_scratch.len() {
            let id = &self.render_scratch[clip_idx];
            let (layer_index, clip_index, trigger_count, anim_progress) =
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

            let ctx = GeneratorContext {
                time,
                beat,
                dt,
                width: self.width,
                height: self.height,
                output_width: self.width,
                output_height: self.height,
                aspect: self.width as f32 / self.height as f32,
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
                // Clear reused render targets to prevent stale content from a
                // previous clip/layer leaking through on the first frame.
                if active.needs_clear {
                    gpu.clear_texture(&active.render_target.texture, 0.0, 0.0, 0.0, 0.0);
                    active.needs_clear = false;
                }
                // Pass per-clip string params (e.g. text content) to the generator.
                // If the clip's map is missing keys that other clips on the layer
                // have set (e.g. fontFamily), fill them from the layer-level cache.
                // Uses cached clip_index for O(1) lookup (set during start_clip).
                let clip_params = layer
                    .clips
                    .get(clip_index as usize)
                    .and_then(|c| c.string_params.as_ref());

                // Update layer defaults from this clip's params (learn new keys).
                // If any new key is learned, mark dirty to rebuild merged cache.
                if let Some(map) = clip_params {
                    for (k, v) in map {
                        if !v.is_empty() && layer_state.layer_string_defaults.get(k) != Some(v) {
                            layer_state
                                .layer_string_defaults
                                .insert(k.clone(), v.clone());
                            layer_state.string_params_dirty = true;
                        }
                    }
                }

                // Merge: use clip params, falling back to layer defaults for
                // missing keys. Use cached merged map — only rebuild when dirty.
                if layer_state.layer_string_defaults.is_empty() {
                    layer_state.generator.set_string_params(clip_params);
                } else {
                    if layer_state.string_params_dirty {
                        layer_state
                            .merged_string_params
                            .clone_from(&layer_state.layer_string_defaults);
                        if let Some(map) = clip_params {
                            for (k, v) in map {
                                layer_state
                                    .merged_string_params
                                    .insert(k.clone(), v.clone());
                            }
                        }
                        layer_state.string_params_dirty = false;
                    }
                    layer_state
                        .generator
                        .set_string_params(Some(&layer_state.merged_string_params));
                }
                let new_progress =
                    layer_state
                        .generator
                        .render(gpu, &active.render_target.texture, &ctx);
                active.anim_progress = new_progress;
            }
        }

        // Flush uniform arena (recreates buffer if capacity grew).
        self.uniform_arena.flush(gpu.device);
        // Clear the arena pointer from GpuEncoder.
        gpu.uniform_arena = None;
    }

    /// Get the animation progress for a rendered clip (for profiling).
    pub fn get_clip_anim_progress(&self, clip_id: &str) -> f32 {
        self.active_clips
            .get(clip_id)
            .map_or(0.0, |a| a.anim_progress)
    }

    /// Get the texture for a rendered clip (used by compositor).
    pub fn get_clip_texture(&self, clip_id: &str) -> Option<&manifold_gpu::GpuTexture> {
        self.active_clips.get(clip_id).map(|a| a.output_texture())
    }

    /// Resize all render targets and generators.
    pub fn resize_gpu(&mut self, width: u32, height: u32, _output_width: u32, _output_height: u32) {
        self.width = width;
        self.height = height;
        // Safety: device_ptr points to GpuDevice owned by ContentPipeline,
        // which outlives GeneratorRenderer. No aliasing with active_clips/generators.
        let device = unsafe { &*self.device_ptr };
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
    pub fn update_active_types_for_layer(&mut self, layer_id: &LayerId, new_type: GeneratorTypeId) {
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
            // Type swap discards the per-layer override — the layer
            // hasn't been re-edited against the new type yet, so the
            // override would refer to the old graph shape. The next
            // `acquire_clip` will re-snapshot the (possibly cleared)
            // override and rebuild if a user edits against the new
            // type. Pass `None` here.
            if let Some(generator) =
                self.registry
                    .create_with_override(self.device(), &new_type, None)
            {
                // Preserve layer_string_defaults across type changes
                let old_defaults = self
                    .layer_generators
                    .get(layer_id)
                    .map(|ls| ls.layer_string_defaults.clone())
                    .unwrap_or_default();
                self.layer_generators.insert(
                    layer_id.clone(),
                    LayerGeneratorState {
                        generator,
                        generator_type: new_type.clone(),
                        trigger_count: old_trigger_count,
                        override_version: None,
                        layer_string_defaults: old_defaults,
                        merged_string_params: std::collections::BTreeMap::new(),
                        string_params_dirty: true,
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
        clip.video_clip_id.is_empty()
    }

    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        _current_time: Seconds,
        layers: &[Layer],
        layer_index: i32,
    ) -> bool {
        // Use the layer_index from the scheduler to get layer_id and generator_type — O(1).
        let layer = layers.get(layer_index as usize);
        let (layer_id, gen_type) = layer
            .map(|l| (l.layer_id.clone(), l.generator_type().clone()))
            .unwrap_or_default();
        // Find clip_index within the layer for O(1) string_params lookup in render_all.
        // This scan runs once per clip start (0-2 per frame), not per-frame.
        let clip_index = layer
            .and_then(|l| l.clips.iter().position(|c| c.id == clip.id))
            .unwrap_or(0) as u32;
        // Per-layer generator graph override + its monotonic version
        // counter. `acquire_clip` rebuilds the generator when the
        // version changes so graph-editor edits actually drive
        // rendering.
        let override_def = layer.and_then(|l| l.generator_graph.as_ref());
        let override_version = layer.map(|l| l.generator_graph_version).unwrap_or(0);
        let acquired = self.acquire_clip(
            &clip.id,
            gen_type,
            layer_id.clone(),
            layer_index,
            clip_index,
            override_def,
            override_version,
        );

        // Populate layer string defaults by scanning ALL clips on this layer.
        // This ensures string params set on any clip (e.g. fontFamily on one clip)
        // are available as defaults for clips that don't have them.
        if acquired
            && let Some(layer_state) = self.layer_generators.get_mut(&layer_id)
            && let Some(layer) = layers.get(layer_index as usize)
        {
            for c in &layer.clips {
                if let Some(map) = &c.string_params {
                    for (k, v) in map {
                        if !v.is_empty() && !layer_state.layer_string_defaults.contains_key(k) {
                            layer_state
                                .layer_string_defaults
                                .insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            // New clip started — merged cache needs rebuild with this clip's params.
            layer_state.string_params_dirty = true;
        }

        acquired
    }

    fn stop_clip(&mut self, clip_id: &str) {
        if let Some(active) = self.active_clips.remove(clip_id) {
            // Return RT to the pool for reuse on the next clip start.
            self.available_rts.push(active.render_target);

            // Layer generator state (generator instance + trigger_count) persists
            // across clip boundaries. This is required for snap parameters to work:
            // trigger_count must accumulate across clips so generators can detect
            // new triggers. Cleanup happens in release_all() or when the generator
            // type changes via update_active_types_for_layer().
        }
    }

    fn release_all(&mut self) {
        for (_, active) in self.active_clips.drain() {
            self.available_rts.push(active.render_target);
        }
        // Release per-layer generator state (particle buffers, density textures, etc.)
        // to prevent GPU memory leaks across project switches.
        self.layer_generators.clear();
        // Drop the pooled render-target Vec too. Across project
        // switches at different resolutions, these would otherwise
        // persist as stale-sized RenderTargets. Lazy-realloc on the
        // next clip start.
        self.available_rts.clear();
        // Force layer_index rescan on next render after project reload.
        self.last_data_version = u64::MAX;
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

    fn resume_clip(&mut self, _clip_id: &str) { /* no-op: generators render every frame */
    }
    fn pause_clip(&mut self, _clip_id: &str) { /* no-op */
    }
    fn seek_clip(&mut self, _clip_id: &str, _video_time: f32) { /* no-op */
    }
    fn set_clip_looping(&mut self, _clip_id: &str, _looping: bool) { /* no-op */
    }
    fn set_clip_playback_rate(&mut self, _clip_id: &str, _rate: f32) { /* no-op */
    }

    fn pre_render(&mut self, _time: Seconds, _beat: Beats, _dt: f32) {
        // No-op: actual GPU rendering is done via render_all() called from app
        // with encoder context that the trait can't provide.
        // Unity's PreRender delegates to RenderAll, but Rust needs explicit GPU context.
    }

    fn resize(&mut self, width: i32, height: i32) {
        let w = width as u32;
        let h = height as u32;
        self.resize_gpu(w, h, w, h);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
