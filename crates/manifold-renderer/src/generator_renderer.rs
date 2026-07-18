use crate::generators::registry::GeneratorRegistry;
use crate::preset_runtime::PresetRuntime;
use crate::gpu_encoder::GpuEncoder;
use crate::preset_context::PresetContext;
use crate::render_target::RenderTarget;
use crate::uniform_arena::UniformArena;
use ahash::AHashMap;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, ClipId, PresetTypeId, LayerId, NodeId, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_playback::renderer::ClipRenderer;
use std::any::Any;
use std::sync::Arc;

/// Per-clip active state.
struct ActiveClip {
    /// Generator renders into this texture at full output resolution.
    render_target: RenderTarget,
    generator_type: PresetTypeId,
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
    generator: Box<PresetRuntime>,
    generator_type: PresetTypeId,
    /// The layer's clip-launch edge counter (existing behavior, unconditional
    /// pre-§8) — bumped in `acquire_clip`, gated by the generator's own
    /// `audio_trigger.mode` wanting `ClipEdge` (§8 D1; no config = always on,
    /// preserving old-project behavior byte-for-byte).
    clip_count: u32,
    /// §8 D1: bumped once per audio-trigger fire this layer's generator (or
    /// any effect in its chain — D5) is configured to react to, mode-gated at
    /// increment time by the firing instance's own `audio_trigger.mode`
    /// wanting `Transient`. See [`Self::effective_trigger_count`].
    audio_count: u32,
    /// `Layer::generator_graph_structure_version` at the time this generator
    /// was constructed. Only a topology change (node/wire add or remove, type
    /// swap, revert) bumps it, so `acquire_clip` / the per-frame sweep rebuild
    /// the generator only when structure actually changed. A value-only edit
    /// (an inner param tweak) bumps the snapshot version instead and is applied
    /// in place — see [`Self::applied_param_version`]. `None` when built from
    /// the bundled preset (no override present at construction time).
    override_version: Option<u32>,
    /// `Layer::generator_graph_version` (the snapshot counter, bumped by every
    /// edit) last reflected into the live graph. When it advances without a
    /// structure change, the sweep pushes the new inner-node param values into
    /// the running generator via `apply_inner_param_overrides` — no rebuild, so
    /// sim/particle state survives. `None` mirrors no override present.
    applied_param_version: Option<u32>,
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
    /// Whether this generator was built unfused because its layer was the
    /// watched (open-in-editor) target. Compared against the live watch state in
    /// the rebuild sweeps so opening/closing the editor flips the generator
    /// fused ⇄ unfused — the registry's fuse gate only re-runs on rebuild.
    built_watched: bool,
    /// `docs/DEPTH_RELIGHT_DESIGN.md` P5: the "3D Shading" toggle + knobs
    /// last reflected into this generator, as `(relight, RelightParams)`.
    /// The template is synthesized at splice time from the live
    /// `PresetInstance`, not authored into `generator_graph` — so neither
    /// `override_version` nor `applied_param_version` sees a toggle flip or
    /// a knob drag. Compared against the layer's current `gen_params` each
    /// sweep to force exactly the rebuild those values need.
    applied_relight: (bool, manifold_core::effects::RelightParams),
}

impl LayerGeneratorState {
    /// §8 D1: the value fed into `PresetContext.trigger_count` / consuming
    /// graphs' `generator_input.trigger_count` — the layer's clip edge plus
    /// its audio-trigger fires. Wrapping add: a `u32` overflow only after
    /// billions of triggers on one layer in one session, and wrapping (not
    /// saturating) matches the existing clip-count overflow policy.
    fn effective_trigger_count(&self) -> u32 {
        self.clip_count.wrapping_add(self.audio_count)
    }
}

/// GPU-side clip renderer for generators.
/// Manages per-layer Generator instances and per-clip RenderTargets.
///
/// All generators render at full output resolution. If a specific generator
/// needs internal downscaling for performance (e.g. raymarching, fluid sim),
/// it does so inside its own `render()` by allocating and managing its own
/// reduced-resolution intermediate textures — the runtime doesn't model it.
/// Thumbnail-resolution dimensions for the §24 5c cold-start render. Rendered at
/// 2× the atlas cell (256×144) so the box-downsample into the cell supersamples
/// — crisper text and edges than a 1:1 render. Still tiny, so the parked-clip
/// thumbnail render stays cheap (~1.2 MB transient target).
const THUMB_W: u32 = 512;
const THUMB_H: u32 = 288;
/// Warm-up frames for a freshly-created cold-start instance (§24 5c-2): stateful
/// generators look empty at t=0, so we advance the runtime this many steps before
/// the parked still is read. ~0.75 s at 60 fps; cheap on the tiny target.
const WARMUP_FRAMES: usize = 45;

/// §24 5c cold-start: an ISOLATED generator instance + small render target for one
/// PARKED clip's thumbnail. Separate from the live per-layer `layer_generators` so
/// rendering a parked clip's thumbnail can never disturb an active clip's state on
/// the same layer.
struct ThumbGen {
    runtime: Box<PresetRuntime>,
    rt: RenderTarget,
    gen_type: PresetTypeId,
}

pub struct GeneratorRenderer {
    /// Shared handle to the GpuDevice owned by ContentPipeline. An `Arc`
    /// clone instead of a cached raw pointer means this survives any future
    /// move of `ContentPipeline`/`ContentThread` (BUG-054).
    device: Arc<GpuDevice>,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    registry: GeneratorRegistry,
    active_clips: AHashMap<ClipId, ActiveClip>,
    layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    /// §24 5c cold-start thumbnail instances, keyed by clip id (parked clips).
    thumb_gens: AHashMap<ClipId, ThumbGen>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<ClipId>,
    /// Per-clip render info: (layer_index, clip_index, trigger_count, anim_progress).
    /// Parallel to render_scratch — avoids LayerId/PresetTypeId clones in render loop.
    render_info_scratch: Vec<(i32, u32, u32, f32)>,
    /// Shared-memory uniform arena for generator uniform data.
    /// Eliminates per-generator queue.write_buffer() calls.
    uniform_arena: UniformArena,
    /// Cached data_version — layer_index refresh scan only runs when this changes.
    last_data_version: u64,
    /// The layer whose generator is currently open in the graph editor (watched),
    /// or `None`. Set each frame by [`Self::set_preview_node`] / cleared by
    /// [`Self::clear_preview`]. A watched generator renders *unfused* so the
    /// node-output preview can sample inner-node textures and edits land live;
    /// the rebuild sweeps below flip the watched layer's generator fused ⇄ unfused
    /// when this changes (mirrors the effect chain's `preview_effect` rebuild key).
    preview_layer: Option<LayerId>,
    /// Per-step GPU/CPU attribution profiling on/off for every generator this
    /// renderer owns (PERF_BUDGET_GATE_DESIGN P2 / D6). Applied to each
    /// generator's executor at chain-insertion time
    /// (`install_layer_generator`) and fanned out to already-live generators
    /// via [`Self::set_profiling`]. `false` costs one `bool` set per
    /// generator per call — zero GPU/CPU timing.
    profiling_enabled: bool,
}

/// This generator's profiled-tag scope: `gen:{layer_id}`.
fn gen_scope(layer_id: &LayerId) -> String {
    format!("gen:{layer_id}")
}

impl GeneratorRenderer {
    pub fn new(
        device: Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        _pool_size: usize,
    ) -> Self {
        // Lazy allocation: start empty, grow on demand as clips start.
        // Avoids pre-allocating large textures that may never be used.
        let available_rts = Vec::with_capacity(8);

        let uniform_arena = UniformArena::new(&device);

        let registry = GeneratorRegistry::new(format);
        // Pre-compile all generator pipelines into the binary archive.
        // Generators are created and immediately dropped — compiled Metal pipeline
        // binaries persist in the archive. Eliminates first-use stutter.
        registry.prewarm_all(&device);

        Self {
            device,
            width,
            height,
            format,
            registry,
            active_clips: AHashMap::with_capacity(16),
            thumb_gens: AHashMap::new(),
            layer_generators: AHashMap::with_capacity(8),
            available_rts,
            render_scratch: Vec::with_capacity(16),
            render_info_scratch: Vec::with_capacity(16),
            uniform_arena,
            last_data_version: u64::MAX, // force scan on first frame
            preview_layer: None,
            profiling_enabled: false,
        }
    }

    /// Enable/disable per-step attribution profiling on every generator this
    /// renderer owns (PERF_BUDGET_GATE_DESIGN P2 / D6). Fans out to
    /// already-installed generators; `install_layer_generator` applies the
    /// same flag to any generator installed after this call.
    pub fn set_profiling(&mut self, on: bool) {
        self.profiling_enabled = on;
        for (layer_id, state) in self.layer_generators.iter_mut() {
            state.generator.set_profiling(on);
            state.generator.set_profile_scope(&gen_scope(layer_id));
        }
    }

    /// Drain every owned generator's per-step CPU profiles recorded on the
    /// last profiled frame.
    pub fn take_step_profiles(&mut self) -> Vec<crate::node_graph::StepProfile> {
        let mut out = Vec::new();
        for state in self.layer_generators.values_mut() {
            out.extend(state.generator.take_step_profiles());
        }
        out
    }

    /// Set the device pointer after the GpuDevice has been moved to its
    /// final location (inside ContentPipeline). Must be called before any
    /// generator is created.
    /// Aim the authoring-time node-output preview at `node_id` within the
    /// generator on `layer_id`, clearing every other layer's generator so a
    /// stale target doesn't pin a texture. Call each frame before
    /// [`Self::render_all`] while the editor watches this generator.
    pub fn set_preview_node(
        &mut self,
        layer_id: &LayerId,
        node_id: Option<&manifold_core::NodeId>,
    ) {
        // Record the watched layer so the next `render_all` rebuild sweep keeps
        // its generator unfused (per-node textures only exist on the unfused
        // path). Cheap clone; only changes when the editor opens/closes/retargets.
        if self.preview_layer.as_ref() != Some(layer_id) {
            self.preview_layer = Some(layer_id.clone());
        }
        for (lid, state) in self.layer_generators.iter_mut() {
            let target = if lid == layer_id { node_id } else { None };
            state.generator.set_preview_node(target);
        }
    }

    /// Clear preview capture AND the thumbnail-atlas dump on every layer's
    /// generator (no preview active). Clearing the dump here too means a closed
    /// editor leaves no generator dumping — a live show pays nothing — without
    /// depending on the unfused→fused executor rebuild to reset it.
    pub fn clear_preview(&mut self) {
        self.preview_layer = None;
        for state in self.layer_generators.values_mut() {
            state.generator.set_preview_node(None);
            state.generator.clear_dump_set();
        }
    }

    /// Set the per-node thumbnail-atlas dump to the editor's currently-visible
    /// nodes on the watched `layer_id`, and CLEAR it on every other layer.
    /// Empty `visible` = atlas off. Touching every layer (mirroring
    /// [`Self::set_preview_node`]) means switching the watched generator, or
    /// scrolling to an empty scope, can't leave a stale dump running on a
    /// non-watched layer — generator dump state is fully explicit each frame, so
    /// a live show never carries it regardless of the executor rebuild gate.
    /// The watched layer is kept unfused by [`Self::set_preview_node`], so the
    /// per-node textures the dump reads exist.
    pub fn set_dump_visible(&mut self, layer_id: &LayerId, visible: &[NodeId]) {
        for (lid, state) in self.layer_generators.iter_mut() {
            if lid == layer_id && !visible.is_empty() {
                state.generator.set_dump_visible(None, visible);
            } else {
                state.generator.clear_dump_set();
            }
        }
    }

    /// §8 D1: bump `layer_id`'s audio-trigger counter by one fire. Called by
    /// the content pipeline for every [`manifold_playback::modulation::TriggerPulse`]
    /// with `layer_id: Some(_)` this tick (mode-gating already happened in the
    /// playback-side evaluator — a pulse only exists here because its
    /// instance's `audio_trigger.mode` wanted `Transient`). A no-op if the
    /// layer has no live generator (e.g. it was deleted the same tick the
    /// pulse fired).
    pub fn bump_audio_count(&mut self, layer_id: &LayerId) {
        if let Some(ls) = self.layer_generators.get_mut(layer_id) {
            ls.audio_count = ls.audio_count.wrapping_add(1);
        }
    }

    /// §8 D1: `layer_id`'s effective `trigger_count` (clip edge + audio
    /// fires) for the content pipeline to feed into that layer's effect
    /// chain's `PresetContext` (D5 — replaces the old pinned 0.0). `0` if the
    /// layer has no live generator.
    pub fn effective_trigger_count_for_layer(&self, layer_id: &LayerId) -> u32 {
        self.layer_generators
            .get(layer_id)
            .map_or(0, |ls| ls.effective_trigger_count())
    }

    /// Every captured Texture2D output of the generator at `layer_id` as
    /// `(node_id, port, type_id, texture)`, after a [`Self::render_all`] with the
    /// dump enabled. The generator counterpart of `Compositor::dump_textures`.
    pub fn dump_textures(
        &self,
        layer_id: &LayerId,
    ) -> Vec<(String, String, String, &manifold_gpu::GpuTexture)> {
        self.layer_generators
            .get(layer_id)
            .map(|s| s.generator.dump_textures_all())
            .unwrap_or_default()
    }

    /// The captured preview texture for the generator on `layer_id`, from the
    /// most recent [`Self::render_all`]. `None` if absent or nothing captured.
    pub fn preview_texture(&self, layer_id: &LayerId) -> Option<&manifold_gpu::GpuTexture> {
        self.layer_generators
            .get(layer_id)?
            .generator
            .preview_texture()
    }

    /// How the watched generator's previewed node should be rendered (flow
    /// wheel / lift / raw). `Color` if the layer has no generator.
    pub fn preview_encoding(&self, layer_id: &LayerId) -> crate::node_graph::PreviewEncoding {
        self.layer_generators
            .get(layer_id)
            .map(|s| s.generator.preview_encoding())
            .unwrap_or_default()
    }

    /// Live scalar I/O of the watched generator's previewed node — for the
    /// editor's value inspector when the node has no image.
    pub fn preview_scalar_io(
        &self,
        layer_id: &LayerId,
    ) -> crate::node_graph::PreviewScalarIo {
        self.layer_generators
            .get(layer_id)
            .map(|s| s.generator.preview_scalar_io())
            .unwrap_or_default()
    }

    /// Live (post-modulation) scalar param values for every node of the watched
    /// generator on `layer_id`, keyed by stable [`NodeId`] — so the editor
    /// canvas reflects what a card slider / driver / Ableton / envelope is doing
    /// to each inner knob this frame, not the frozen authoring def. Empty if the
    /// layer has no generator.
    pub fn live_node_params(&self, layer_id: &LayerId) -> crate::node_graph::LiveNodeParams {
        self.layer_generators
            .get(layer_id)
            .map(|s| s.generator.live_node_params_watched())
            .unwrap_or_default()
    }

    /// Get a reference to the GpuDevice.
    fn device(&self) -> &GpuDevice {
        &self.device
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
    #[allow(clippy::too_many_arguments)]
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: PresetTypeId,
        layer_id: LayerId,
        layer_index: i32,
        clip_index: u32,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: u32,
        param_version: u32,
        clip_edge_enabled: bool,
        // The layer's live per-instance manifest, forwarded to
        // `install_layer_generator` when this clip start triggers a build so
        // the reshape sources from the manifest, not the stale shadow (BUG-078).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5) — the layer's live
        // toggle + knobs.
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool {
        if self.active_clips.contains_key(clip_id) {
            return true;
        }

        // Compare against the layer's current generator state. Rebuild
        // when: (a) no generator yet, (b) type changed, (c) the override
        // version differs from what we last built against (graph-editor edit
        // landed), or (d) the "3D Shading" toggle/knobs changed (P5 — the
        // relight template is synthesized at splice time, invisible to the
        // override/param version counters). `None` here means "no override
        // present this frame"; `Some(v)` means "override is at v". Encoding
        // "no override" as `None` lets us distinguish the initial
        // bundled-preset path from a v0 override.
        let current_override_version: Option<u32> =
            override_def.map(|_| override_version);
        let current_param_version: Option<u32> = override_def.map(|_| param_version);
        let is_watched_now = self.preview_layer.as_ref() == Some(&layer_id);
        let needs_create = self
            .layer_generators
            .get(&layer_id)
            .is_none_or(|ls| {
                ls.generator_type != gen_type
                    || ls.override_version != current_override_version
                    || ls.built_watched != is_watched_now
                    || ls.applied_relight.0 != relight
                    || ls.applied_relight.1.height_from != relight_params.height_from
            });

        if needs_create {
            // Preserve `clip_count`/`audio_count` across rebuild. Without
            // this, editing the override graph mid-clip OR changing the
            // generator type resets the counters to 0, which makes the
            // next clip-trigger's `count % N` calculation potentially
            // collide with the value the previous instance just
            // emitted — the user sees the same pattern back-to-back
            // even though the math should never produce duplicates.
            // The counter is conceptually "how many times has this
            // layer been triggered" (clip launches + audio fires, §8 D1)
            // and is generator-agnostic, so carrying both forward is
            // semantically correct.
            let (preserved_clip_count, preserved_audio_count) = self
                .layer_generators
                .get(&layer_id)
                .map(|ls| (ls.clip_count, ls.audio_count))
                .unwrap_or((0, 0));
            if !self.install_layer_generator(
                layer_id.clone(),
                gen_type.clone(),
                override_def,
                current_override_version,
                current_param_version,
                preserved_clip_count,
                preserved_audio_count,
                std::collections::BTreeMap::new(),
                manifest,
                relight,
                relight_params,
            ) {
                return false;
            }
        }

        // §8 D1: the clip-launch edge is mode-gated at increment time by the
        // generator's own `audio_trigger.mode` (no config = always on,
        // preserving pre-§8 behavior byte-for-byte for every project that
        // hasn't touched this feature).
        if clip_edge_enabled && let Some(ls) = self.layer_generators.get_mut(&layer_id) {
            ls.clip_count = ls.clip_count.wrapping_add(1);
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
        // Layer indices to skip rendering entirely this frame: hidden behind a
        // full-opacity Opaque layer and safe to not render (content pipeline's
        // `compute_render_skip_indices`). Their generators don't dispatch and
        // their sim state simply pauses — safe because the occluder gate lets
        // them resume before they can be seen again. Empty = render everything.
        render_skip: &[i32],
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
            let current_override_version: Option<u32> = layer
                .generator_graph()
                .map(|_| layer.generator_graph_structure_version());
            let current_param_version: Option<u32> =
                layer.generator_graph().map(|_| layer.generator_graph_version());
            // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5): compared
            // below alongside `override_version`/`built_watched` — see
            // `acquire_clip`'s doc comment on the same comparison.
            let (current_relight, current_relight_params) = layer
                .gen_params()
                .map(|gp| (gp.relight_active(), gp.relight_params))
                .unwrap_or_default();
            // Rebuild only on a *structure* change (override structure-version
            // bump), a "3D Shading" toggle/knob change, OR when the layer's
            // watched state flipped (editor opened/closed — swaps the
            // generator fused ⇄ unfused). A value-only edit lands in place
            // below, with no rebuild and no state reset.
            let is_watched_now = self.preview_layer.as_ref() == Some(layer_id);
            let needs_rebuild = self.layer_generators.get(layer_id).is_some_and(|ls| {
                ls.override_version != current_override_version
                    || ls.built_watched != is_watched_now
                    || ls.applied_relight != (current_relight, current_relight_params)
            });
            if !needs_rebuild {
                // Value-only edit (inner param tweak): push the new values into
                // the live generator without tearing it down, so sim/particle
                // state survives.
                if let Some(ls) = self.layer_generators.get_mut(layer_id)
                    && ls.applied_param_version != current_param_version
                {
                    if let Some(def) = layer.generator_graph() {
                        ls.generator.apply_inner_param_overrides(def);
                    }
                    ls.applied_param_version = current_param_version;
                }
                continue;
            }
            let (preserved_clip_count, preserved_audio_count) = self
                .layer_generators
                .get(layer_id)
                .map(|ls| (ls.clip_count, ls.audio_count))
                .unwrap_or((0, 0));
            let gen_type = layer.generator_type().clone();
            let override_def = layer.generator_graph();
            // Structural rebuild via the per-frame sweep: hand the live
            // manifest so a reshape recalibrated since the last save wins
            // over the graph's stale shadow (BUG-078).
            let manifest = layer.gen_params().map(|gp| &gp.params);
            self.install_layer_generator(
                layer_id.clone(),
                gen_type,
                override_def,
                current_override_version,
                current_param_version,
                preserved_clip_count,
                preserved_audio_count,
                std::collections::BTreeMap::new(),
                manifest,
                current_relight,
                current_relight_params,
            );
        }

        // Collect clip IDs into pre-allocated scratch to avoid borrow conflict
        self.render_scratch.clear();
        self.render_scratch
            .extend(self.active_clips.keys().cloned());

        // Pre-collect (layer_index, trigger_count, anim_progress, internal_scale)
        // per clip during immutable borrow, avoiding per-clip LayerId/PresetTypeId clones.
        self.render_info_scratch.clear();
        for id in &self.render_scratch {
            if let Some(active) = self.active_clips.get(id.as_str()) {
                let trigger_count = self
                    .layer_generators
                    .get(&active.layer_id)
                    .map_or(0, |ls| ls.effective_trigger_count());
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
            // Render-skip: this layer is hidden behind a full-opacity Opaque
            // layer and safe to not render at all. Skip the generator dispatch
            // — its render target keeps its last frame (never blended while
            // occluded) and its sim state pauses until the layer is revealed.
            if render_skip.contains(&layer_index) {
                continue;
            }

            // Skip-mode (parity with the effect chain's `is_skipped_for`): a
            // generator can declare skip-when-zero in its preset metadata. For
            // a source there's no upstream to pass through, so a skipped
            // generator renders a transparent frame — the compositor then
            // shows the layers below. No shipping generator declares skip today
            // (default `SkipMode::Never`), so this is a no-op until one does.
            let skipped = layers
                .get(layer_index as usize)
                .and_then(|l| l.gen_params())
                .is_some_and(|gp| {
                    crate::node_graph::loaded_preset_view_by_id(gp.generator_type()).is_some_and(
                        |view| {
                            crate::node_graph::is_skipped_for(view.skip_mode, &view.type_id, gp)
                        },
                    )
                });
            if skipped {
                if let Some(active) = self.active_clips.get_mut(id.as_str()) {
                    gpu.clear_texture(&active.render_target.texture, 0.0, 0.0, 0.0, 0.0);
                    active.needs_clear = false;
                }
                continue;
            }

            let ctx = PresetContext {
                time,
                beat,
                dt,
                width: self.width,
                height: self.height,
                output_width: self.width,
                output_height: self.height,
                aspect: self.width as f32 / self.height as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress,
                trigger_count,
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
                // Apply the layer's per-instance reshape notes before
                // rendering. Version-gated inside the generator, so a
                // note-free layer pays one integer compare per frame; a
                // note edit rebuilds the affected reshapes + clears the
                // apply-cache so it takes effect immediately. Downstream
                // only — never touches the value slots modulation writes.
                // The generator's id-keyed slider manifest drives the bindings
                // by source_id; empty when the layer has no generator instance.
                let empty = ParamManifest::default();
                let params = layer.gen_params().map(|gp| &gp.params).unwrap_or(&empty);
                let relight_params = layer
                    .gen_params()
                    .map(|gp| gp.relight_params)
                    .unwrap_or_default();
                layer_state
                    .generator
                    .set_relight_params(&relight_params);
                let new_progress = layer_state.generator.render(
                    gpu,
                    &active.render_target.texture,
                    &ctx,
                    params,
                );
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
        // Clone the Arc (cheap refcount bump) so `device` doesn't borrow
        // `self` — otherwise the immutable self-borrow would conflict with
        // the mutable active_clips/generators borrows below.
        let device = Arc::clone(&self.device);
        let device = &*device;
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
        let device = Arc::clone(&self.device);
        let device = &*device;
        for layer_state in self.layer_generators.values_mut() {
            layer_state.generator.reset_state(device);
        }
    }

    /// BUG-104 — release trigger-EDGE latch state on every live generator,
    /// leaving particle sims / feedback / accumulators untouched. The
    /// narrow sibling of [`Self::reset_all_generator_state`]: a
    /// `LayerGeneratorState`'s generator is deliberately long-lived across
    /// clip changes on the same layer (temporal continuity), so a full
    /// reset on every transport stop would be its own regression. Trigger
    /// latches (`node.sample_and_hold`, `node.clip_trigger_cycle`,
    /// `node.clip_trigger_index`, `node.frequency_ratio`,
    /// `node.cycle_table_row`, `node.trigger_gate`, `node.trigger_ease_to`)
    /// have no such continuity expectation — see
    /// `PresetRuntime::clear_trigger_state` for the mechanism. Call from
    /// the same "kill the trigger" moments
    /// `manifold_playback::modulation::clear_all_trigger_edges` already
    /// fires (transport stop, project load).
    pub fn clear_all_trigger_state(&mut self) {
        for layer_state in self.layer_generators.values_mut() {
            layer_state.generator.clear_trigger_state();
        }
    }

    /// Update active clip types for a layer after generator type change.
    /// Port of C# GeneratorRenderer.UpdateActiveTypesForLayer().
    pub fn update_active_types_for_layer(&mut self, layer_id: &LayerId, new_type: PresetTypeId) {
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
            let (old_clip_count, old_audio_count) = self
                .layer_generators
                .get(layer_id)
                .map_or((0, 0), |ls| (ls.clip_count, ls.audio_count));
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
                None,
                old_clip_count,
                old_audio_count,
                old_defaults,
                // Fresh type → bundled build; the old instance's manifest
                // describes the old param set, so no manifest to honor here.
                None,
                // Same reasoning for "3D Shading": a type swap installs a
                // fresh `PresetInstance` (`ChangeGeneratorTypeCommand`), so
                // there's no live relight state to carry over here. If the
                // new instance actually does carry a toggle, the very next
                // per-frame sweep compares against it and rebuilds again —
                // a harmless one-frame redundancy, same shape as the `None`
                // manifest above.
                false,
                manifold_core::effects::RelightParams::default(),
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
    #[allow(clippy::too_many_arguments)]
    fn install_layer_generator(
        &mut self,
        layer_id: LayerId,
        gen_type: PresetTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: Option<u32>,
        param_version: Option<u32>,
        clip_count: u32,
        audio_count: u32,
        layer_string_defaults: std::collections::BTreeMap<String, String>,
        // The layer's live per-instance param manifest (`gen_params.params`),
        // threaded into the generator build so a post-calibration rebuild
        // sources each param's reshape range/curve/invert from the manifest
        // authority, not the graph's stale `preset_metadata.params` shadow
        // (BUG-078). `None` for the type-swap path (fresh bundled build).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5).
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool {
        // A layer open in the graph editor renders unfused (per-node preview +
        // live edits). The registry's fuse gate consults this; the rebuild
        // sweeps consult `built_watched` to re-instantiate when it toggles.
        let is_watched = self.preview_layer.as_ref() == Some(&layer_id);
        let Some(mut generator) = self.registry.create_with_override(
            Arc::clone(&self.device),
            &gen_type,
            override_def,
            self.width,
            self.height,
            is_watched,
            manifest,
            relight.then_some(&relight_params),
        ) else {
            return false;
        };
        // D6 correction: apply the current profiling flag + this generator's
        // scope at chain-insertion time, so a freshly (re)built generator
        // never misses a --profile run in progress.
        generator.set_profiling(self.profiling_enabled);
        generator.set_profile_scope(&gen_scope(&layer_id));
        self.layer_generators.insert(
            layer_id.clone(),
            LayerGeneratorState {
                generator,
                generator_type: gen_type,
                clip_count,
                audio_count,
                override_version,
                applied_param_version: param_version,
                layer_string_defaults,
                merged_string_params: std::collections::BTreeMap::new(),
                string_params_dirty: true,
                built_watched: is_watched,
                applied_relight: (relight, relight_params),
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

    /// §24 5c cold-start: render a PARKED generator clip's thumbnail into an
    /// ISOLATED thumbnail-resolution target and return it. Shows the generator's
    /// default look at `time`/`beat` with the clip's authored (base) params —
    /// NOT modulation, override-graph edits, or warm-up state, none of which are
    /// computed off the playhead. The live look replaces it the moment the clip
    /// plays (the P1 snapshot). Uses a separate generator instance per clip so it
    /// can never disturb an active clip's state on the same layer. Cheap (tiny
    /// target); the caller bounds how many per frame. Returns `None` if the layer
    /// has no generator params.
    pub fn render_clip_thumbnail(
        &mut self,
        gpu: &mut GpuEncoder,
        clip_id: &str,
        layer: &Layer,
        clip_index: u32,
        time: f64,
        beat: f64,
    ) -> Option<&manifold_gpu::GpuTexture> {
        let gp = layer.gen_params()?;
        let gen_type = gp.generator_type().clone();

        let needs_create = self
            .thumb_gens
            .get(clip_id)
            .is_none_or(|t| t.gen_type != gen_type);
        if needs_create {
            let runtime = self.registry.create_with_override(
                Arc::clone(&self.device),
                &gen_type,
                None,
                THUMB_W,
                THUMB_H,
                false,
                // Cold-start thumbnail shows the bundled default look, not
                // live override/calibration state (see fn doc) — no manifest,
                // and no "3D Shading" either (`docs/DEPTH_RELIGHT_DESIGN.md`
                // P5): same policy as the manifest above, same reasoning.
                None,
                None,
            )?;
            let rt = RenderTarget::new(
                self.device(),
                THUMB_W,
                THUMB_H,
                self.format,
                "Generator Thumbnail RT",
            );
            self.thumb_gens.insert(
                ClipId::new(clip_id),
                ThumbGen {
                    runtime,
                    rt,
                    gen_type: gen_type.clone(),
                },
            );
        }

        let string_params = layer
            .clips
            .get(clip_index as usize)
            .and_then(|c| c.string_params.as_ref());

        // A freshly-created instance is warmed up: stateful generators (fluid sims,
        // feedback) need several frames before they look like anything, so a single
        // t=0 render is the empty/uninteresting frame. We advance the runtime
        // `WARMUP_FRAMES` steps (state accumulates in the persistent runtime) so the
        // parked still is a developed look. Cheap — a tiny target, ≤1 new instance
        // per frame is enforced by the caller. A later refresh continues from the
        // warm state, so it stays warm.
        const DT: f64 = 1.0 / 60.0;
        let frames = if needs_create { WARMUP_FRAMES } else { 1 };

        let t = self.thumb_gens.get_mut(clip_id)?;
        t.runtime.set_string_params(string_params);
        gpu.clear_texture(&t.rt.texture, 0.0, 0.0, 0.0, 0.0);
        for f in 0..frames {
            let ctx = PresetContext {
                time: time + f as f64 * DT,
                beat,
                dt: DT as f32,
                width: THUMB_W,
                height: THUMB_H,
                output_width: THUMB_W,
                output_height: THUMB_H,
                aspect: THUMB_W as f32 / THUMB_H as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: f as i64,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            t.runtime.render(gpu, &t.rt.texture, &ctx, &gp.params);
        }
        Some(&t.rt.texture)
    }

    /// The cold-start thumbnail texture for `clip_id`, if one has been rendered.
    /// Separate from `render_clip_thumbnail` so the caller can render several
    /// (each a `&mut self` call) and then collect their textures by shared borrow.
    pub fn thumb_texture(&self, clip_id: &str) -> Option<&manifold_gpu::GpuTexture> {
        self.thumb_gens.get(clip_id).map(|t| &t.rt.texture)
    }

    /// Drop cold-start thumbnail instances for clips no longer requested, bounding
    /// memory (each holds a generator instance + a small render target). Takes the
    /// visible clip slice directly — no per-frame set allocation; `thumb_gens` holds
    /// at most a handful of entries, so the linear `contains` is negligible.
    pub fn evict_thumb_gens(&mut self, keep: &[ClipId]) {
        if self.thumb_gens.len() != keep.len() {
            self.thumb_gens.retain(|k, _| keep.contains(k));
        }
    }
}

// =====================================================================
// IClipRenderer implementation
// Port of C# GeneratorRenderer : IClipRenderer
// =====================================================================

impl ClipRenderer for GeneratorRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        // A generator clip carries neither a video source nor an image
        // source. Image clips also have an empty `video_clip_id`, so they
        // must be excluded explicitly — `ImageRenderer` claims them.
        clip.video_clip_id.is_empty() && clip.image_path.is_empty()
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
        // Per-layer generator graph override + its version counters. The
        // generator rebuilds only on a *structure* change; a value-only edit
        // bumps the snapshot `param_version` and is applied in place.
        let override_def = layer.and_then(|l| l.generator_graph());
        let override_version = layer
            .map(|l| l.generator_graph_structure_version())
            .unwrap_or(0);
        let param_version = layer.map(|l| l.generator_graph_version()).unwrap_or(0);
        // §9 U3 (formerly §8 D1): the clip edge is mode-gated by the
        // generator's OWN fire-mode audio mod, if any (no such mod = always
        // on — old-project behavior, unchanged). `Transient`-only mode
        // silently drops the clip-launch contribution for this layer's
        // trigger_count. `PresetInstance::clip_edge_enabled()` owns the
        // disabled-means-absent rule; don't read a mod's `trigger_mode`
        // directly.
        let clip_edge_enabled = layer
            .and_then(|l| l.gen_params())
            .map(|gp| gp.clip_edge_enabled())
            .unwrap_or(true);
        // The layer's live per-instance manifest — its `spec`s are the reshape
        // authority a first-clip build must honor over the graph shadow
        // (BUG-078). Borrowed from the external `layers` slice, not `self`.
        let manifest = layer.and_then(|l| l.gen_params()).map(|gp| &gp.params);
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5): the toggle +
        // knobs live on `gen_params` alongside the manifest above.
        let (relight, relight_params) = layer
            .and_then(|l| l.gen_params())
            .map(|gp| (gp.relight_active(), gp.relight_params))
            .unwrap_or_default();
        let acquired = self.acquire_clip(
            &clip.id,
            gen_type,
            layer_id.clone(),
            layer_index,
            clip_index,
            override_def,
            override_version,
            param_version,
            clip_edge_enabled,
            manifest,
            relight,
            relight_params,
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
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
            device.arc(),
            host_w,
            host_h,
            GpuTextureFormat::Rgba16Float,
            0,
        );

        let layer_id = LayerId::new("layer-under-test");
        let other_layer = LayerId::new("other-layer");
        let trivial = PresetTypeId::new("TrivialPassthrough");
        let strange = PresetTypeId::new("StrangeAttractor");

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
                None,
                0,
                0,
                std::collections::BTreeMap::new(),
                None,
                false,
                manifold_core::effects::RelightParams::default(),
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
            let json_gen = layer_state.generator.as_ref();
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

    /// §8 D1 — the generator half of the P2 gate (the effect-chain half is
    /// `preset_runtime::generator_input_tests::run_feeds_nonzero_trigger_count_into_generator_input_effect_slot`).
    /// `effective_trigger_count` sums `clip_count` (clip-launch edge) +
    /// `audio_count` (audio-trigger fires), and the clip edge is mode-gated
    /// at `acquire_clip` time by `clip_edge_enabled` — `Transient`-only mode
    /// (simulated here by passing `false`) must NOT bump `clip_count`.
    #[test]
    fn effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode() {
        let device = crate::test_device();
        let mut renderer = GeneratorRenderer::new(device.arc(), 256, 256, GpuTextureFormat::Rgba16Float, 0);
        let layer_id = LayerId::new("trigger-count-layer");
        let gen_type = PresetTypeId::new("TrivialPassthrough");

        assert!(
            renderer.install_layer_generator(
                layer_id.clone(),
                gen_type.clone(),
                None,
                None,
                None,
                0,
                0,
                std::collections::BTreeMap::new(),
                None,
                false,
                manifold_core::effects::RelightParams::default(),
            ),
            "seed install must succeed",
        );

        // Two clip launches (clip_edge_enabled = true, the default/no-config
        // behavior): clip_count 0 -> 2.
        assert!(renderer.acquire_clip(
            "clip-1",
            gen_type.clone(),
            layer_id.clone(),
            0,
            0,
            None,
            0,
            0,
            true,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert!(renderer.acquire_clip(
            "clip-2",
            gen_type.clone(),
            layer_id.clone(),
            0,
            1,
            None,
            0,
            0,
            true,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert_eq!(
            renderer.layer_generators.get(&layer_id).unwrap().clip_count,
            2,
            "two distinct clip launches with clip edge enabled must bump clip_count twice",
        );

        // Three audio-trigger fires: audio_count 0 -> 3.
        renderer.bump_audio_count(&layer_id);
        renderer.bump_audio_count(&layer_id);
        renderer.bump_audio_count(&layer_id);
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&layer_id),
            5,
            "effective count must be clip_count(2) + audio_count(3) = 5",
        );

        // A third clip launch with clip_edge_enabled = false (Transient-only
        // mode) must NOT bump clip_count — the whole point of D1's mode gate.
        assert!(renderer.acquire_clip(
            "clip-3",
            gen_type,
            layer_id.clone(),
            0,
            2,
            None,
            0,
            0,
            false,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert_eq!(
            renderer.layer_generators.get(&layer_id).unwrap().clip_count,
            2,
            "Transient-only mode must silently ignore the clip-launch edge",
        );
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&layer_id),
            5,
            "effective count unchanged by the mode-gated-off clip launch",
        );

        // A layer with no generator reads 0, not a panic.
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&LayerId::new("no-such-layer")),
            0,
        );
    }
}
