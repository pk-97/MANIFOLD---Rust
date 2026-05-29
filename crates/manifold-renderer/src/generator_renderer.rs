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
            if !self.install_layer_generator(
                layer_id.clone(),
                gen_type.clone(),
                override_def,
                current_override_version,
                preserved_trigger_count,
                std::collections::BTreeMap::new(),
            ) {
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
            self.install_layer_generator(
                layer_id.clone(),
                gen_type,
                override_def,
                current_override_version,
                preserved_trigger_count,
                std::collections::BTreeMap::new(),
            );
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
                    params[i] = val.value;
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
            // Preserve layer_string_defaults across type changes.
            let old_defaults = self
                .layer_generators
                .get(layer_id)
                .map(|ls| ls.layer_string_defaults.clone())
                .unwrap_or_default();
            // Type swap discards the per-layer override — the layer
            // hasn't been re-edited against the new type yet, so the
            // override would refer to the old graph shape. The next
            // `acquire_clip` will re-snapshot the (possibly cleared)
            // override and rebuild if a user edits against the new
            // type. Pass `None` here.
            self.install_layer_generator(
                layer_id.clone(),
                new_type.clone(),
                None,
                None,
                old_trigger_count,
                old_defaults,
            );
        }
    }

    /// Single funnel for "a new `Generator` instance now owns rendering
    /// for `layer_id`." Every rebuild path — first-clip acquire, per-frame
    /// override-version sweep, user-driven generator type swap — routes
    /// through here so two invariants hold by construction:
    ///
    /// 1. The new generator is built at the host's *current* canvas
    ///    dimensions (`self.width` × `self.height`), so the JSON chain
    ///    builder's `canvas_sized_array_outputs` pre-allocation (scatter
    ///    accumulators, density grids, future ping-pong sims) lands at
    ///    the right pixel count on the very first frame. Before this
    ///    centralization, sites called the registry directly with
    ///    hardcoded 1920×1080, never followed up with `resize()`, and
    ///    the splat buffer stayed sized for a sub-rect of the real
    ///    canvas — the "Strange Attractor renders in the top-left
    ///    quadrant after generator swap" bug.
    ///
    /// 2. Every `ActiveClip` for this layer is marked `needs_clear`,
    ///    so the canvas-sized output texture is wiped to opaque black
    ///    before the new generator writes to it. Without this the
    ///    previous generator's last frame stays visible wherever the
    ///    new generator doesn't write (e.g. a particle generator with
    ///    sparse splats leaves the previous shape generator's bright
    ///    rectangles bleeding through — the second half of the same
    ///    visual bug).
    ///
    /// Returns `true` on successful install. Returns `false` only if
    /// the registry rejected the construction (unknown type / preset
    /// failed to load); in that case the existing entry (if any) is
    /// left untouched so the previous generator keeps rendering.
    fn install_layer_generator(
        &mut self,
        layer_id: LayerId,
        gen_type: GeneratorTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: Option<u32>,
        trigger_count: u32,
        layer_string_defaults: std::collections::BTreeMap<String, String>,
    ) -> bool {
        let Some(generator) = self.registry.create_with_override(
            self.device(),
            &gen_type,
            override_def,
            self.width,
            self.height,
        ) else {
            return false;
        };
        self.layer_generators.insert(
            layer_id.clone(),
            LayerGeneratorState {
                generator,
                generator_type: gen_type,
                trigger_count,
                override_version,
                layer_string_defaults,
                merged_string_params: std::collections::BTreeMap::new(),
                string_params_dirty: true,
            },
        );
        // Mark every active clip on this layer for a clear before the
        // first render against the freshly installed generator. The
        // canvas-sized render target may still hold the previous
        // generator's last frame; without this, a sparse new generator
        // (particles, wireframes) leaves the old generator's pixels
        // visible wherever the new one doesn't write.
        for active in self.active_clips.values_mut() {
            if active.layer_id == layer_id {
                active.needs_clear = true;
            }
        }
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::json_graph_generator::JsonGraphGenerator;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    /// Architectural regression: a generator type swap mid-clip must
    /// re-build the per-layer `Generator` against the host's *current*
    /// canvas dimensions AND mark every active clip on that layer for
    /// a render-target clear before the new generator's first frame.
    ///
    /// Pre-fix, `update_active_types_for_layer` and the per-frame
    /// override-version sweep called the registry directly with
    /// hardcoded 1920×1080 and never touched `ActiveClip::needs_clear`.
    /// Two visible failure modes:
    /// 1. At any host resolution other than 1920×1080, the new
    ///    generator's `canvas_sized_array_outputs` (scatter
    ///    accumulators, density grids) allocated at 1920×1080 and the
    ///    dispatch sized from `Backend::canvas_dims()` mapped splats
    ///    into a sub-rect of the real canvas — Strange Attractor
    ///    rendered into the top-left quadrant only.
    /// 2. The canvas-sized output texture still held the previous
    ///    generator's last frame; wherever the new generator didn't
    ///    write (sparse particle splats, narrow wireframes), the old
    ///    generator's pixels stayed visible — the user-reported
    ///    "leaves an artifact of the previous generator" bug.
    ///
    /// Both invariants now hold by construction because
    /// `install_layer_generator` is the only path that mutates
    /// `layer_generators`, and it (a) passes `self.width/height` into
    /// `GeneratorRegistry::create_with_override` (which takes canvas
    /// dims as required arguments — no silent default), and (b)
    /// dirties every `active_clips` entry for the affected layer.
    ///
    /// This test exercises the *swap* path (the one that was
    /// fundamentally broken) at a non-default host resolution.
    #[test]
    fn generator_type_swap_marks_active_clips_for_clear_at_host_canvas_dims() {
        let device = crate::test_device();
        let host_w: u32 = 1280;
        let host_h: u32 = 720;

        let mut renderer = GeneratorRenderer::new(
            &device,
            host_w,
            host_h,
            GpuTextureFormat::Rgba16Float,
            0,
        );

        let layer_id = LayerId::new("layer-under-test");
        let other_layer = LayerId::new("other-layer");
        let trivial = GeneratorTypeId::new("TrivialPassthrough");
        let strange = GeneratorTypeId::new("ComputeStrangeAttractor");

        // Seed a `LayerGeneratorState` for the starting type via the
        // same funnel any production path would use. (Any JSON preset
        // works; the test doesn't render it, it just exists so the
        // swap path has something to replace.)
        assert!(
            renderer.install_layer_generator(
                layer_id.clone(),
                trivial.clone(),
                None,
                None,
                0,
                std::collections::BTreeMap::new(),
            ),
            "seed install of TrivialPassthrough must succeed",
        );

        // Two active clips on the layer at non-default canvas dims.
        // Manually populated so the test doesn't depend on the
        // `start_clip` plumbing — the invariant under test is that
        // `install_layer_generator` reaches every active clip on the
        // layer regardless of how it got there.
        for tag in ["clip-a", "clip-b"] {
            let rt = RenderTarget::new(
                &device,
                host_w,
                host_h,
                GpuTextureFormat::Rgba16Float,
                "test RT",
            );
            renderer.active_clips.insert(
                ClipId::new(tag),
                ActiveClip {
                    render_target: rt,
                    generator_type: trivial.clone(),
                    layer_id: layer_id.clone(),
                    layer_index: 0,
                    clip_index: 0,
                    anim_progress: 0.0,
                    // Pretend the first frame already cleared the
                    // flag. The swap must re-dirty both clips.
                    needs_clear: false,
                },
            );
        }
        // One clip on a *different* layer that must NOT be touched
        // by the swap. Catches the "iterate every active clip"
        // footgun (over-clearing other layers).
        let other_rt = RenderTarget::new(
            &device,
            host_w,
            host_h,
            GpuTextureFormat::Rgba16Float,
            "other RT",
        );
        renderer.active_clips.insert(
            ClipId::new("clip-other"),
            ActiveClip {
                render_target: other_rt,
                generator_type: trivial.clone(),
                layer_id: other_layer.clone(),
                layer_index: 1,
                clip_index: 0,
                anim_progress: 0.0,
                needs_clear: false,
            },
        );

        // === The swap ===
        renderer.update_active_types_for_layer(&layer_id, strange.clone());

        // Invariant 1: the rebuild used host canvas dims, not a
        // hardcoded default. If a future regression silently drops
        // the dims again, this assertion fails before any visual bug
        // can ship.
        {
            let layer_state = renderer
                .layer_generators
                .get_mut(&layer_id)
                .expect("layer state must exist after swap");
            assert_eq!(
                layer_state.generator_type, strange,
                "swap must install the new generator type",
            );
            let json_gen = layer_state
                .generator
                .as_any()
                .downcast_ref::<JsonGraphGenerator>()
                .expect(
                    "ComputeStrangeAttractor must be a JSON-backed generator for this regression \
                     — if it has moved back to a Rust factory, update this assertion",
                );
            assert_eq!(
                json_gen.backend_for_test().canvas_dims(),
                (host_w, host_h),
                "post-swap generator's backend must report host canvas dims, \
                 not the registry's pre-fix hardcoded default",
            );
        }

        // Invariant 2: every active clip on the swapped layer is
        // dirty; the unrelated layer's clip is untouched.
        let clip_a = renderer
            .active_clips
            .get("clip-a")
            .expect("clip-a must remain active after swap");
        assert!(
            clip_a.needs_clear,
            "clip-a on the swapped layer must be marked for clear",
        );
        let clip_b = renderer
            .active_clips
            .get("clip-b")
            .expect("clip-b must remain active after swap");
        assert!(
            clip_b.needs_clear,
            "clip-b on the swapped layer must be marked for clear",
        );
        let clip_other = renderer
            .active_clips
            .get("clip-other")
            .expect("clip-other must remain active");
        assert!(
            !clip_other.needs_clear,
            "swap on layer-under-test must not touch clips on other layers",
        );
    }
}
