use manifold_node_engine::runtime::generator_provider::{GeneratorProvider, generator_provider};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::runtime::preset_context::{PresetContext, ProjectTempo};
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::gpu::uniform_arena::UniformArena;
use ahash::AHashMap;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, ClipId, PresetTypeId, LayerId, NodeId, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_playback::renderer::ClipRenderer;
use std::any::Any;
use std::sync::Arc;

use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::water::fluid::FluidDomainSnapshot;
use manifold_node_engine::scene::scene_viewport::{SceneViewportConfig, SceneViewportHostError};
use manifold_node_engine::runtime::ModifierPreviewContext;

mod physics_events;
#[cfg(not(any(test, feature = "testkit")))]
mod state;
#[cfg(any(test, feature = "testkit"))]
pub mod state;
use state::{ActiveClip, LayerGeneratorState, ThumbGen};
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;



/// Prepared replacements remain private until every pipeline owner has admitted resize.
pub struct PreparedGeneratorResize {
    width: u32,
    height: u32,
    active: Vec<(ClipId, RenderTarget)>,
    available: Vec<RenderTarget>,
    layers: Vec<(LayerId, manifold_node_engine::runtime::PreparedRuntimeResize)>,
}



/// GPU-side clip renderer for generators.
/// Manages per-layer Generator instances and per-clip RenderTargets.
///
/// All generators render at full output resolution. If a specific generator
/// needs internal downscaling for performance (e.g. raymarching, fluid sim),
/// it does so inside its own `render()` by allocating and managing its own
/// reduced-resolution intermediate textures — the runtime doesn't model it.
/// Thumbnail-resolution dimensions for the section 24 5c cold-start render. Rendered at
/// 2× the atlas cell (256×144) so the box-downsample into the cell supersamples
/// — crisper text and edges than a 1:1 render. Still tiny, so the parked-clip
/// thumbnail render stays cheap (~1.2 MB transient target).
const THUMB_W: u32 = 512;
const THUMB_H: u32 = 288;
/// Warm-up frames for a freshly-created cold-start instance (section 24 5c-2): stateful
/// generators look empty at t=0, so we advance the runtime this many steps before
/// the parked still is read. ~0.75 s at 60 fps; cheap on the tiny target.
const WARMUP_FRAMES: usize = 45;


pub struct GeneratorRenderer {
    next_physics_event: u64,
    scene_impulse_diagnostics: manifold_node_engine::water::runtime::scene_impulses::SceneImpulseDiagnostics,
    /// Shared handle to the GpuDevice owned by ContentPipeline. An `Arc`
    /// clone instead of a cached raw pointer means this survives any future
    /// move of `ContentPipeline`/`ContentThread` (BUG-054).
    #[cfg(not(any(test, feature = "testkit")))]
    device: Arc<GpuDevice>,
    #[cfg(any(test, feature = "testkit"))]
    pub device: Arc<GpuDevice>,
    width: u32,
    height: u32,
    #[cfg(not(any(test, feature = "testkit")))]
    format: GpuTextureFormat,
    #[cfg(any(test, feature = "testkit"))]
    pub format: GpuTextureFormat,
    #[cfg(not(any(test, feature = "testkit")))]
    registry: &'static GeneratorProvider,
    #[cfg(any(test, feature = "testkit"))]
    pub registry: &'static GeneratorProvider,
    #[cfg(not(any(test, feature = "testkit")))]
    active_clips: AHashMap<ClipId, ActiveClip>,
    #[cfg(any(test, feature = "testkit"))]
    pub active_clips: AHashMap<ClipId, ActiveClip>,
    #[cfg(not(any(test, feature = "testkit")))]
    layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    #[cfg(any(test, feature = "testkit"))]
    pub layer_generators: AHashMap<LayerId, LayerGeneratorState>,
    /// section 24 5c cold-start thumbnail instances, keyed by clip id (parked clips).
    #[cfg(not(any(test, feature = "testkit")))]
    thumb_gens: AHashMap<ClipId, ThumbGen>,
    #[cfg(any(test, feature = "testkit"))]
    pub thumb_gens: AHashMap<ClipId, ThumbGen>,
    available_rts: Vec<RenderTarget>,
    /// Pre-allocated scratch buffer for render iteration (avoids per-frame alloc).
    render_scratch: Vec<ClipId>,
    /// Per-clip render info: (layer_index, clip_index, trigger_count, anim_progress).
    /// Parallel to render_scratch — avoids LayerId/PresetTypeId clones in render loop.
    render_info_scratch: Vec<(i32, u32, u32, f32)>,
    /// Shared-memory uniform arena for generator uniform data.
    /// Eliminates per-generator queue.write_buffer() calls.
    #[cfg(not(any(test, feature = "testkit")))]
    uniform_arena: UniformArena,
    #[cfg(any(test, feature = "testkit"))]
    pub uniform_arena: UniformArena,
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
    /// Per-frame RT quality, pushed from project settings via the
    /// `ClipRenderer::set_rt_quality` fan-out. Applied to each generator's
    /// `PresetRuntime` at render time (same D6-correction discipline as
    /// `dispatch_chain`) so runtimes installed after the last push still
    /// render at the current quality.
    rt_quality: manifold_node_engine::exec::effect_node::RtQuality,
    /// SCENE_FX P4a — borrowed pointer to the compositor's layer-skin registry
    /// for this frame. Set by the host before `render_all`; a raw pointer is
    /// used because the renderer's lifetime is independent of the registry.
    layer_skin_registry: Option<manifold_node_engine::runtime::layer_skin::LayerSkinPtr>,
    /// Render-only viewport request forwarded to the matching generator
    /// runtime. The modifier context is shared with the host and does not
    /// participate in graph execution.
    scene_viewport_request: Option<(LayerId, NodeId, SceneViewportConfig)>,
    scene_viewport_modifier: Option<Arc<ModifierPreviewContext>>,
    scene_viewport_error: Option<SceneViewportHostError>,
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
        pool_size: usize,
    ) -> Self {
        let renderer = Self::new_unwarmed(device, width, height, format, pool_size);
        (renderer.registry.prewarm)(&renderer.device, renderer.format);
        renderer
    }

    /// Prewarm is the app's no-hitch guarantee; tests use this constructor to compile lazily.
    pub fn new_unwarmed(
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

        let registry = generator_provider();

        Self {
            next_physics_event: 0,
            scene_impulse_diagnostics: Default::default(),
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
            rt_quality: manifold_node_engine::exec::effect_node::RtQuality::default(),
            layer_skin_registry: None,
            scene_viewport_request: None,
            scene_viewport_modifier: None,
            scene_viewport_error: None,
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
    pub fn take_step_profiles(&mut self) -> Vec<manifold_node_engine::exec::execution::StepProfile> {
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
        self.scene_viewport_request = None;
        self.scene_viewport_modifier = None;
        self.scene_viewport_error = None;
        for state in self.layer_generators.values_mut() {
            state.generator.set_preview_node(None);
            state.generator.clear_dump_set();
            state.generator.clear_scene_viewport();
        }
    }

    /// Store and immediately apply the render-only scene viewport request to
    /// currently-live runtimes. Rebuilt runtimes receive it again immediately
    /// before their generator render in [`Self::render_all`].
    pub fn set_scene_viewport_request(
        &mut self,
        request: Option<(LayerId, NodeId, SceneViewportConfig)>,
        modifier: Option<Arc<ModifierPreviewContext>>,
    ) {
        self.scene_viewport_request = request;
        self.scene_viewport_modifier = modifier;
        self.scene_viewport_error = None;
        self.clear_scene_viewport_runtimes();
        self.apply_scene_viewport_to_live_runtime();
    }

    fn clear_scene_viewport_runtimes(&mut self) {
        let requested_layer = self
            .scene_viewport_request
            .as_ref()
            .map(|(layer_id, _, _)| layer_id);
        for (layer_id, state) in self.layer_generators.iter_mut() {
            if requested_layer != Some(layer_id) {
                state.generator.clear_scene_viewport();
            }
        }
    }

    fn apply_scene_viewport_to_live_runtime(&mut self) {
        let Some((layer_id, node_id, config)) = self.scene_viewport_request.as_ref() else {
            return;
        };
        let modifier = self.scene_viewport_modifier.clone();
        let error = {
            let Some(state) = self.layer_generators.get_mut(layer_id) else {
                return;
            };
            let result = if let Some(context) = modifier.as_deref() {
                state
                    .generator
                    .set_modifier_scene_viewport(context, node_id, *config)
            } else {
                state
                    .generator
                    .set_scene_viewport_watched(node_id, *config)
                    .map_err(SceneViewportHostError::InvalidTarget)
            };
            let error = result.err();
            if error.is_some() {
                state.generator.clear_scene_viewport();
            }
            error
        };
        if let Some(error) = error {
            self.scene_viewport_error = Some(error);
        } else {
            self.scene_viewport_error = None;
        }
    }

    /// The latest valid viewport color for `layer_id`.
    pub fn scene_viewport_texture(
        &self,
        layer_id: &LayerId,
    ) -> Option<&manifold_gpu::GpuTexture> {
        if self.scene_viewport_error.is_some()
            || self.scene_viewport_request.as_ref().is_none_or(|(requested, _, _)| requested != layer_id)
        {
            return None;
        }
        self.layer_generators
            .get(layer_id)
            .and_then(|state| state.generator.scene_viewport_texture())
    }

    pub fn scene_viewport_status(
        &self,
        layer_id: &LayerId,
    ) -> Result<FrameRenderStatus, SceneViewportHostError> {
        if let Some(error) = self.scene_viewport_error {
            return Err(error);
        }
        let Some((requested, _, _)) = self.scene_viewport_request.as_ref() else {
            return Err(SceneViewportHostError::MissingRuntime);
        };
        if requested != layer_id {
            return Err(SceneViewportHostError::MissingRuntime);
        }
        let Some(state) = self.layer_generators.get(layer_id) else {
            return Err(SceneViewportHostError::MissingRuntime);
        };
        state
            .generator
            .scene_viewport_status()
            .ok_or(SceneViewportHostError::MissingRuntime)
    }

    pub fn write_scene_viewport_fluid_domains(
        &self,
        layer_id: &LayerId,
        output: &mut Vec<(NodeId, FluidDomainSnapshot)>,
    ) {
        if self.scene_viewport_error.is_some()
            || self.scene_viewport_request.as_ref().is_none_or(|(requested, _, _)| requested != layer_id)
        {
            return;
        }
        let Some(state) = self.layer_generators.get(layer_id) else {
            return;
        };
        if let Some(context) = self.scene_viewport_modifier.as_deref() {
            state.generator.write_modifier_fluid_domains(context, output);
        } else {
            state.generator.write_fluid_domains_watched(output);
        }
    }

    /// Keep local modifier addresses out of the host's node namespace.
    pub fn set_modifier_preview_node(
        &mut self,
        layer_id: &LayerId,
        context: Option<&manifold_node_engine::runtime::ModifierPreviewContext>,
        node: Option<&NodeId>,
    ) -> Option<manifold_node_engine::runtime::ModifierPreviewError> {
        self.set_preview_node(layer_id, None);
        let runtime = &mut self.layer_generators.get_mut(layer_id)?.generator;
        match context {
            Some(context) => runtime.set_modifier_preview_node(context, node).err(),
            None => node.map(|_| manifold_node_engine::runtime::ModifierPreviewError::MissingNode),
        }
    }

    /// SCENE_FX P4a — set the borrowed layer-skin registry for this frame.
    /// The registry must outlive `render_all` (content thread guarantee).
    /// `None` clears the pointer.
    pub fn set_layer_skin_registry(&mut self, registry: Option<&manifold_node_engine::runtime::layer_skin::LayerSkinRegistry>) {
        self.layer_skin_registry = registry.map(manifold_node_engine::runtime::layer_skin::LayerSkinPtr::new);
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

    pub fn set_modifier_dump_visible(
        &mut self, layer_id: &LayerId,
        context: Option<&manifold_node_engine::runtime::ModifierPreviewContext>, visible: &[NodeId],
    ) {
        for (lid, state) in &mut self.layer_generators {
            if lid == layer_id && let Some(context) = context {
                state.generator.set_dump_visible_with_context(None, visible, Some(context));
            } else {
                state.generator.clear_dump_set();
            }
        }
    }

    pub fn modifier_preview_local_node(
        &self, layer_id: &LayerId, context: &manifold_node_engine::runtime::ModifierPreviewContext,
        generated: &str,
    ) -> Option<&NodeId> {
        self.layer_generators.get(layer_id)?.generator.modifier_preview_local_node(context, generated)
    }

    /// section 8 D1: bump `layer_id`'s audio-trigger counter by one fire. Called by
    /// the content pipeline for every [`manifold_playback::modulation::TriggerPulse`]
    /// with `layer_id: Some(_)` this tick (mode-gating already happened in the
    /// playback-side evaluator — a pulse only exists here because its
    /// instance's `audio_trigger.mode` wanted `Transient`). A no-op if the
    /// layer has no live generator (e.g. it was deleted the same tick the
    /// pulse fired).
    pub fn bump_audio_count(&mut self, layer_id: &LayerId) {
        if let Some(ls) = self.layer_generators.get_mut(layer_id) {
            ls.generator.note_trigger_event(ls.effective_trigger_count());
            ls.audio_count = ls.audio_count.wrapping_add(1);
        }
    }

    /// Modifier-owned audio stays in that modifier's stream. Legacy host and
    /// effect gates retain the existing layer counter behavior.
    pub fn route_audio_pulse(&mut self, layer_id: &LayerId, owner: &manifold_core::EffectId, param_key: u64) {
        if let Some(layer) = self.layer_generators.get_mut(layer_id)
            && layer.event_owner.as_ref() == Some(owner)
            && layer.generator.note_modifier_audio_key(param_key)
        {
            return;
        }
        self.bump_audio_count(layer_id);
    }

    /// section 8 D1: `layer_id`'s effective `trigger_count` (clip edge + audio
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
    pub fn preview_encoding(&self, layer_id: &LayerId) -> manifold_node_engine::preview_encoding::PreviewEncoding {
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
    ) -> manifold_node_engine::preview_encoding::PreviewScalarIo {
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
    pub fn live_node_params(&self, layer_id: &LayerId) -> manifold_node_engine::preview_encoding::LiveNodeParams {
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
        modifier_event: Option<(&manifold_core::effects::PresetInstance, bool)>,
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
        // Keyed on the manifest's presence (not the override's) so a
        // spec-only mapping edit on a catalog-default generator — no
        // override, just a `graph_version` bump — is visible to the sweep's
        // value-only path, matching the sweep's own keying below.
        let current_param_version: Option<u32> = manifest.map(|_| param_version);
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
            // layer been triggered" (clip launches + audio fires, section 8 D1)
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

        // section 8 D1: the clip-launch edge is mode-gated at increment time by the
        // generator's own `audio_trigger.mode` (no config = always on,
        // preserving pre-section 8 behavior byte-for-byte for every project that
        // hasn't touched this feature).
        if let Some(ls) = self.layer_generators.get_mut(&layer_id) {
            let clip_edge_enabled = modifier_event.map_or(clip_edge_enabled, |(host, fire)| {
                fire && host.clip_edge_enabled_matching(|param| !ls.generator.is_modifier_trigger_param(param))
            });
            if let Some((host, fire_clip_edge)) = modifier_event {
                ls.event_owner = Some(host.id.clone());
                if fire_clip_edge { ls.generator.note_modifier_clip_event(Some(host)); }
            }
            if clip_edge_enabled {
                ls.generator.note_trigger_event(ls.effective_trigger_count());
                ls.clip_count = ls.clip_count.wrapping_add(1);
            }
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
        project_tempo: Option<&ProjectTempo>,
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
            // Keyed on `gen_params` (not the override graph): a spec-only
            // mapping edit on a catalog-default generator bumps
            // `graph_version` without materializing an override, and this
            // counter is the only signal that edit produces.
            let current_param_version: Option<u32> =
                layer.gen_params().map(|_| layer.generator_graph_version());
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
                    // BUG-18l: a live forced-outputs change (rt_enabled /
                    // temporal_upscale) is a topology change — the compiled
                    // plan can't honor it, so rebuild the runtime here, the
                    // path that already preserves clip counts and harvests
                    // state.
                    || ls.generator.awaiting_forced_outputs_rebuild()
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
                    // Re-bake the binding reshapes from the live manifest so a
                    // mapping-spec edit (curve/invert/range) reaches the inner
                    // nodes in place. This is the ONLY channel for a spec-only
                    // edit on a catalog-default generator — no override is
                    // materialized, so no rebuild ever follows.
                    if let Some(gp) = layer.gen_params() {
                        ls.generator
                            .apply_manifest_reshape(&gp.params, layer.generator_graph());
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

        // Captures are valid only for a runtime that is actually rendered on
        // this frame. Skipped layers must not retain the previous frame's
        // viewport texture while the matching rebuilt runtime is re-aimed
        // below immediately before its render.
        self.clear_scene_viewport_runtimes();
        // Re-aim the matching runtime even when its layer has no visible clip
        // this frame; set_scene_viewport invalidates its capture flag without
        // dropping the pass or its render history.
        self.apply_scene_viewport_to_live_runtime();

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

            gpu.checkpoint();

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
                // BUG-84fv: name the card in the "Generators" buffer's fault
                // log — the buffer covers every generator of the frame, so a
                // bare buffer label can't say which card hung.
                gpu.native_enc.note_scope(&format!(
                    "gen L{layer_index} {} clip {}",
                    active.generator_type, id
                ));
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
                // The generator's id-keyed slider manifest drives the bindings
                // by source_id; empty when the layer has no generator instance.
                // Values only — the reshape half (range/curve/invert) is
                // re-baked from this same manifest by the per-frame sweep above
                // on a `graph_version` bump, not here.
                let empty = ParamManifest::default();
                let params = layer.gen_params().map(|gp| &gp.params).unwrap_or(&empty);
                let relight_params = layer
                    .gen_params()
                    .map(|gp| gp.relight_params)
                    .unwrap_or_default();
                layer_state
                    .generator
                    .set_relight_params(&relight_params);
                layer_state.generator.set_rt_quality(self.rt_quality);
                layer_state
                    .generator
                    .set_layer_skin_registry(self.layer_skin_registry.map(|p| unsafe { p.get() }));
                layer_state.generator.set_project_tempo(project_tempo);
                layer_state.generator.set_physics_source_instance(layer.gen_params());
                let new_progress = layer_state.generator.render(
                    gpu,
                    &active.render_target.texture,
                    &ctx,
                    params,
                );
                active.anim_progress = new_progress;
                // Acknowledge native tick-start receipts every rendered frame;
                // otherwise completed clicks would fill the bounded event queue.
                let diagnostics = &mut self.scene_impulse_diagnostics;
                layer_state.generator.drain_scene_impulses(|_, event| {
                    diagnostics.started = diagnostics.started.saturating_add(1);
                    if event.lateness.0 > 0.0 {
                        diagnostics.late = diagnostics.late.saturating_add(1);
                    }
                });
                layer_state.generator.drain_discarded_scene_impulses(|_, _| {
                    diagnostics.discarded = diagnostics.discarded.saturating_add(1);
                });
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

    /// Prepare all layers without replacing any live target or runtime.
    pub fn prepare_resize_gpu(&self, width: u32, height: u32) -> Result<PreparedGeneratorResize, String> {
        let device = &*self.device;
        let target = |old: &RenderTarget| RenderTarget::try_new(
            device, width, height, old.format, "generator-resize",
        );
        let active = self.active_clips.iter().map(|(id, clip)| {
            Ok((id.clone(), target(&clip.render_target)?))
        }).collect::<Result<_, String>>()?;
        let available = self.available_rts.iter().map(target).collect::<Result<_, _>>()?;
        let layers = self.layer_generators.iter().map(|(id, layer)| {
            layer.generator.prepare_resize(device, width, height)
                .map(|prepared| (id.clone(), prepared)).map_err(|error| error.to_string())
        }).collect::<Result<_, _>>()?;
        Ok(PreparedGeneratorResize { width, height, active, available, layers })
    }

    /// Called on the content thread before another frame can observe the pipeline.
    pub fn commit_resize_gpu(&mut self, prepared: PreparedGeneratorResize) {
        for (id, target) in prepared.active {
            let active = self.active_clips.get_mut(&id).expect("resize owner unchanged");
            active.render_target = target;
            active.needs_clear = true;
        }
        self.available_rts = prepared.available;
        for (id, runtime) in prepared.layers {
            self.layer_generators.get_mut(&id).expect("resize owner unchanged")
                .generator.commit_resize(runtime);
        }
        self.width = prepared.width;
        self.height = prepared.height;
    }

    pub fn resize_gpu(&mut self, width: u32, height: u32, _output_width: u32, _output_height: u32) -> Result<(), String> {
        let prepared = self.prepare_resize_gpu(width, height)?;
        self.commit_resize_gpu(prepared);
        Ok(())
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

        // Swapping to "no generator" removes the instance: there is no
        // NONE preset to install, and falling through to the swap would
        // warn in the registry and leave the old generator rendering.
        if new_type.is_none() {
            self.layer_generators.remove(layer_id);
            return;
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
        let Some(mut generator) = (self.registry.create)(
            Arc::clone(&self.device),
            self.format,
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
        let event_owner = self.layer_generators.get(&layer_id).and_then(|prior| prior.event_owner.clone());
        if let Some(prior) = self.layer_generators.get_mut(&layer_id)
            && prior.generator_type == gen_type
        {
            generator.carry_generator_state_from(&mut prior.generator);
        }
        self.layer_generators.insert(
            layer_id.clone(),
            LayerGeneratorState {
                generator,
                event_owner,
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

    /// section 24 5c cold-start: render a PARKED generator clip's thumbnail into an
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
        // The NONE sentinel is not a preset — a layer whose gen_params
        // outlived its generator has no default look to thumbnail, and
        // asking the registry for it warns on every retry.
        if gen_type.is_none() {
            return None;
        }

        let needs_create = self
            .thumb_gens
            .get(clip_id)
            .is_none_or(|t| t.gen_type != gen_type);
        if needs_create {
            let runtime = (self.registry.create)(
                Arc::clone(&self.device),
                self.format,
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
                    ready: false,
                    frame_count: 0,
                    last_frame_status: manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
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
        t.ready = false;
        t.runtime.set_string_params(string_params);
        t.runtime.set_project_tempo(None);
        t.runtime.set_physics_source_instance(Some(gp));
        gpu.clear_texture(&t.rt.texture, 0.0, 0.0, 0.0, 0.0);
        for _ in 0..frames {
            let frame_count = t.frame_count;
            t.frame_count = t.frame_count.saturating_add(1);
            let ctx = PresetContext {
                time: time + frame_count as f64 * DT,
                beat,
                dt: DT as f32,
                width: THUMB_W,
                height: THUMB_H,
                output_width: THUMB_W,
                output_height: THUMB_H,
                aspect: THUMB_W as f32 / THUMB_H as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            t.runtime.render(gpu, &t.rt.texture, &ctx, &gp.params);
        }
        t.last_frame_status = gpu.frame_status();
        t.ready = !t.runtime.warmup_pending()
            && t.last_frame_status.presentable();
        t.ready.then_some(&t.rt.texture)
    }

    /// The cold-start thumbnail texture for `clip_id`, if one has been rendered.
    /// Separate from `render_clip_thumbnail` so the caller can render several
    /// (each a `&mut self` call) and then collect their textures by shared borrow.
    pub fn thumb_texture(&self, clip_id: &str) -> Option<&manifold_gpu::GpuTexture> {
        self.thumb_gens
            .get(clip_id)
            .filter(|t| {
                t.ready
                    && !t.runtime.warmup_pending()
                    && t.last_frame_status.presentable()
            })
            .map(|t| &t.rt.texture)
    }

    /// Drop cold-start thumbnail instances rejected by the caller's visibility /
    /// capture predicate, bounding memory without touching live layer generators.
    pub fn evict_thumb_gens(&mut self, mut keep: impl FnMut(&ClipId) -> bool) {
        self.thumb_gens.retain(|k, _| keep(k));
    }
}

// =====================================================================
// IClipRenderer implementation
// Port of C# GeneratorRenderer : IClipRenderer
// =====================================================================

impl ClipRenderer for GeneratorRenderer {
    fn set_rt_quality(&mut self, column: &manifold_core::settings::RtQualityColumn) {
        self.rt_quality = manifold_node_engine::exec::effect_node::RtQuality::from_column(column);
    }

    fn can_handle(&self, clip: &TimelineClip) -> bool {
        // A generator clip carries no media source. Image and audio clips
        // also have an empty `video_clip_id`, so they must be excluded
        // explicitly — `ImageRenderer` claims images, and audio clips are
        // driven by `audio_layer_playback`, never by a pixel renderer.
        clip.video_clip_id.is_empty() && clip.image_path.is_empty() && !clip.is_audio()
    }

    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        _current_time: Seconds,
        layers: &[Layer],
        layer_index: i32,
        fire_clip_edge: bool,
    ) -> bool {
        // Use the layer_index from the scheduler to get layer_id and generator_type — O(1).
        let layer = layers.get(layer_index as usize);
        let (layer_id, gen_type) = layer
            .map(|l| (l.layer_id.clone(), l.generator_type().clone()))
            .unwrap_or_default();
        // A media-less clip on a layer with no generator (empty arrangement
        // clip) renders nothing. Bail BEFORE the registry lookup: the NONE
        // sentinel is not a preset, so the attempt would warn and fail —
        // and since a failed start never enters `active_clip_ids`, the
        // scheduler re-issues it every frame, spamming the log for the
        // clip's whole duration. The `false` retry that remains is two
        // field checks, silent and cheap.
        if gen_type.is_none() {
            return false;
        }
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
        // section 9 U3 (formerly section 8 D1): the clip edge is mode-gated by the
        // generator's OWN fire-mode audio mod, if any (no such mod = always
        // on — old-project behavior, unchanged). `Transient`-only mode
        // silently drops the clip-launch contribution for this layer's
        // trigger_count. `PresetInstance::clip_edge_enabled()` owns the
        // disabled-means-absent rule; don't read a mod's `trigger_mode`
        // directly. P3: a layer-drag heal start (`fire_clip_edge=false`)
        // never counts as an edge either — a drag is not a trigger.
        let clip_edge_enabled = fire_clip_edge
            && layer
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
            layer.and_then(|layer| layer.gen_params()).map(|host| (host, fire_clip_edge)),
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

    fn prewarm_layer(
        &mut self,
        layer: &Layer,
        run: manifold_core::WarmupRun,
    ) -> manifold_core::WarmupOutcome {
        if let Some(outcome) = run.exhausted(std::time::Instant::now(), 0) {
            return outcome;
        }
        let budget = run.budget;
        let layer_id = layer.layer_id.clone();
        let gen_type = layer.generator_type().clone();
        if gen_type.is_none() {
            return manifold_core::WarmupOutcome::Quiescent;
        }

        // Collect layer-level string defaults from every clip, matching
        // `start_clip`'s first-touch behavior so the warm generator sees the
        // same params as the first live clip launch. A layer with no clips
        // still warms bare — the defaults map stays empty and is passed to
        // `install_layer_generator` directly.
        let mut layer_string_defaults = std::collections::BTreeMap::new();
        for c in &layer.clips {
            if let Some(map) = &c.string_params {
                for (k, v) in map {
                    if !v.is_empty() {
                        layer_string_defaults.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let override_def = layer.generator_graph();
        let override_version = layer.generator_graph_structure_version();
        let param_version = layer.generator_graph_version();
        let current_override_version: Option<u32> = override_def.map(|_| override_version);
        let (relight, relight_params) = layer
            .gen_params()
            .map(|gp| (gp.relight_active(), gp.relight_params))
            .unwrap_or_default();
        let manifest = layer.gen_params().map(|gp| &gp.params);
        let current_param_version: Option<u32> = manifest.map(|_| param_version);

        // D11: activate the layer's first clip through the production
        // `start_clip` path before rendering, so clip-context topology
        // (including any clip-post-fx chain state) is built during warmup
        // and not on stage. `fire_clip_edge=false` keeps the clip-launch
        // trigger counter clean — the post-warmup trigger-state re-clear
        // handles the rest.
        let first_clip = layer.clips.first();
        if let Some(clip) = first_clip {
            let layers = std::slice::from_ref(layer);
            if !self.start_clip(clip, Seconds(0.0), layers, 0, false) {
                return manifold_core::WarmupOutcome::InstallFailed;
            }
        } else if !self.install_layer_generator(
            layer_id.clone(),
            gen_type,
            override_def,
            current_override_version,
            current_param_version,
            0,
            0,
            layer_string_defaults.clone(),
            manifest,
            relight,
            relight_params,
        ) {
            return manifold_core::WarmupOutcome::InstallFailed;
        }

        // Pre-build the merged string-params cache so `render()` sees the same
        // map the per-frame sweep would build for an active clip.
        let first_clip_params = first_clip.and_then(|c| c.string_params.as_ref());
        if let Some(ls) = self.layer_generators.get_mut(&layer_id) {
            ls.merged_string_params.clone_from(&ls.layer_string_defaults);
            if let Some(map) = first_clip_params {
                for (k, v) in map {
                    ls.merged_string_params.insert(k.clone(), v.clone());
                }
            }
            ls.string_params_dirty = false;
        }

        let device = Arc::clone(&self.device);
        let mut scratch = RenderTarget::new(
            &device,
            self.width,
            self.height,
            self.format,
            "warmup scratch",
        );

        const DT: f64 = 1.0 / 60.0;
        let default_manifest = ParamManifest::default();
        let mut outcome = manifold_core::WarmupOutcome::BudgetExhausted {
            cap: manifold_core::WarmupCap::PerLayerFrames,
            elapsed: std::time::Duration::ZERO,
        };
        let mut warmup_frame_status: manifold_node_engine::runtime::frame_status::FrameRenderStatus;
        for frame in 0..budget.per_layer_frames {
            // Wall-clock is the primary per-layer cap; the frame cap is only
            // a safety bound for runaway spin loops.
            let pump_start = std::time::Instant::now();
            if let Some(exhausted) = run.exhausted(std::time::Instant::now(), frame) {
                outcome = exhausted;
                break;
            }

            self.uniform_arena.reset();
            let mut native_enc = device.create_encoder("warmup");
            native_enc.note_scope(&format!(
                "warmup layer '{}' ({}) generator {} clip {} frame {} size {}x{}",
                layer.name,
                layer_id,
                layer.generator_type(),
                first_clip.map_or("none", |clip| clip.id.as_str()),
                frame,
                self.width,
                self.height,
            ));
            {
                let mut gpu = GpuEncoder::new(&mut native_enc, &device);
                gpu.preparing = true;
                gpu.uniform_arena = Some(&mut self.uniform_arena as *mut UniformArena);
                if let Some(ls) = self.layer_generators.get_mut(&layer_id) {
                    let params = layer
                        .gen_params()
                        .map(|gp| &gp.params)
                        .unwrap_or(&default_manifest);
                    ls.generator.set_string_params(Some(&ls.merged_string_params));
                    ls.generator.set_relight_params(&relight_params);
                    ls.generator.set_rt_quality(self.rt_quality);
                    ls.generator.set_project_tempo(None);
                    ls.generator.set_physics_source_instance(layer.gen_params());
                    let ctx = PresetContext {
                        time: frame as f64 * DT,
                        beat: 0.0,
                        dt: DT as f32,
                        width: self.width,
                        height: self.height,
                        output_width: self.width,
                        output_height: self.height,
                        aspect: self.width as f32 / self.height as f32,
                        owner_key: 0,
                        is_clip_level: false,
                        frame_count: frame as i64,
                        anim_progress: 0.0,
                        trigger_count: 0,
                    };
                    gpu.clear_texture(&scratch.texture, 0.0, 0.0, 0.0, 0.0);
                    ls.generator.render(&mut gpu, &scratch.texture, &ctx, params);
                }
                // §5.4: pending geometry is incomplete preparation — the
                // wrapper's status gates quiescence below.
                warmup_frame_status = gpu.frame_status();
            }
            if let Err(err) = native_enc.try_commit_and_wait_completed() {
                log::error!("Generator warmup failed for layer {layer_id}: {err}");
                return manifold_core::WarmupOutcome::GpuFailed;
            }
            self.uniform_arena.flush(&device);

            let runtime = self.layer_generators.get(&layer_id);
            if let Some(done) = run.pending_outcome(
                runtime.is_some_and(|ls| ls.generator.warmup_pending()),
                warmup_frame_status.presentable(),
                runtime.is_some(),
            ) {
                outcome = done;
                break;
            }

            // Paced wait: if async work is still in flight, yield so the
            // background threads (GLB parse, accel build) can land without
            // burning a whole frame budget on spin-rendered no-ops.
            let delay = run.pending_pump_delay(std::time::Instant::now(), pump_start.elapsed());
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }

        // Recycle the scratch target at the current canvas size.
        scratch.resize(&device, self.width, self.height);
        self.available_rts.push(scratch);

        if matches!(outcome, manifold_core::WarmupOutcome::BudgetExhausted { .. }) {
            outcome = run.exhausted(std::time::Instant::now(), budget.per_layer_frames).unwrap();
        }
        outcome
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
        // BUG-256: this is also the correctness boundary — `layer_generators`
        // is keyed by `LayerId` and rebuild-gated on serialized version
        // counters, both of which collide across template-derived projects,
        // so keeping it would serve the previous project's generators.
        self.layer_generators.clear();
        // A stale preview layer across projects can keep a layer unfused and
        // force a rebuild on first launch — clear it with the rest of the
        // project-derived state.
        self.preview_layer = None;
        // Parked-clip thumbnails are keyed by `ClipId` — same collision
        // class as `layer_generators`, so they must not survive either.
        self.thumb_gens.clear();
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
        if let Err(error) = self.resize_gpu(w, h, w, h) {
            log::error!("Generator resize rejected; keeping previous configuration: {error}");
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
