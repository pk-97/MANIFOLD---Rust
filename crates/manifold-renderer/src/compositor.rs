use crate::gpu_encoder::GpuEncoder;
use crate::layer_compositor::CompositeClipDescriptor;
use crate::tonemap::TonemapSettings;
use manifold_core::BlendMode;
use manifold_core::LayerId;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::{EffectId, NodeId};

/// Per-layer metadata passed to the compositor.
pub struct CompositeLayerDescriptor<'a> {
    pub layer_index: i32,
    pub layer_id: &'a LayerId,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub is_muted: bool,
    pub is_solo: bool,
    pub blit_to_led: bool,
    pub effects: &'a [PresetInstance],
    pub effect_groups: &'a [EffectGroup],
    /// Parent group layer ID (None for root layers).
    pub parent_layer_id: Option<&'a LayerId>,
    /// Whether this layer is a group container.
    pub is_group: bool,
    /// §8 D1/D5: this layer's effective `trigger_count` (clip edge + audio
    /// fires), read from `GeneratorRenderer::effective_trigger_count_for_layer`
    /// and fed into this layer's effect chain's `PresetContext` — the same
    /// value the layer's own generator graph sees. `0` for a layer with no
    /// generator (nothing to be effective about) or group layers (D5 doesn't
    /// define a group-scoped count; deferred).
    pub trigger_count: u32,
}

/// Frame context passed to the compositor each tick.
pub struct CompositorFrame<'a> {
    pub time: f64,
    pub beat: f64,
    pub dt: f32,
    pub frame_count: u64,
    pub compositor_dirty: bool,
    pub clips: &'a [CompositeClipDescriptor<'a>],
    pub layers: &'a [CompositeLayerDescriptor<'a>],
    pub master_effects: &'a [PresetInstance],
    pub master_effect_groups: &'a [EffectGroup],
    /// §8 D5: master/global chains have no owning layer, so their effective
    /// `trigger_count` is audio-fires-only (clip contribution is always 0
    /// here) — accumulated by the content pipeline across every
    /// `TriggerPulse { layer_id: None }` and fed into the master chain's
    /// `PresetContext` the same way a layer's count feeds its chain.
    pub master_trigger_count: u32,
    /// Tonemap settings for this frame.
    pub tonemap: TonemapSettings,
    /// LED exit path index: 0 = capture pre-tonemap composite for LED output,
    /// -1 = use final output (default). Also gates the per-layer LED composite's
    /// tonemap + master FX: index 0 routes the raw composite (no tonemap, no
    /// FX) so master effects that break LEDs are bypassed; -1 applies them.
    pub led_exit_index: i32,
    /// LED grid dimensions (strip_count, leds_per_strip). The per-layer LED
    /// composite is built at this resolution so each strip maps 1:1 to one
    /// column and each LED to one row — no edge-extend transform needed.
    pub led_composite_size: (u32, u32),
    /// Final output dimensions after upscaling. Used by effects that must be
    /// resolution-invariant (edge detect texel size, glitch/dither pixel counts).
    pub output_width: u32,
    pub output_height: u32,
    /// Layer indices occluded this frame by a fully-opaque layer above them
    /// (computed once by the content pipeline; see
    /// `compute_occluded_layer_indices`). Blend-skip ONLY: these layers
    /// render normally (clips, generators, effect chains all run — no state
    /// ever depends on visibility); the compositor just elides their final
    /// blend dispatch, which the opaque layer would overwrite anyway.
    pub occluded_layers: &'a [i32],
    /// Subset of `occluded_layers` whose generators AND effect chains are
    /// skipped entirely this frame, not just their blend (see
    /// `compute_render_skip_indices` in the content pipeline). These layers
    /// produce no `LayerOutput` — they are neither rendered nor blended.
    /// Safe because the opaque occluder gate (Opaque blend at opacity 1.0)
    /// guarantees a skipped layer resumes rendering before it can become
    /// visible again. Empty when the optimization is off or a preview is open.
    pub render_skip: &'a [i32],
}

impl<'a> CompositorFrame<'a> {
    /// Find the layer descriptor matching a given layer_index.
    /// Layers are typically indexed sequentially (0..N), so try direct
    /// positional lookup first (O(1)) before falling back to linear scan.
    #[inline]
    pub fn find_layer(&self, layer_index: i32) -> Option<&CompositeLayerDescriptor<'a>> {
        // Fast path: layers are usually ordered by index, so position == index.
        if let Some(ld) = self.layers.get(layer_index as usize)
            && ld.layer_index == layer_index
        {
            return Some(ld);
        }
        // Slow path: layer order doesn't match index (reordered/gaps).
        self.layers.iter().find(|l| l.layer_index == layer_index)
    }

    /// Whether this layer is occluded by a fully-opaque layer above it.
    /// Occluded layers skip their final blend dispatch only (the opaque
    /// blend overwrites their pixels anyway); they render normally upstream.
    #[inline]
    pub fn is_occluded(&self, layer_index: i32) -> bool {
        self.occluded_layers.contains(&layer_index)
    }
}

/// One dumped texture borrowed from a compositor: `(node_id, port, type_id,
/// texture)`. The strings are owned; the texture borrows the compositor.
pub type DumpTextureRef<'a> = (String, String, String, &'a manifold_gpu::GpuTexture);

/// What to capture from the watched effect's chain on the next `render`.
/// `All` is the Cmd+D one-shot disk dump (every node). `Visible` is the
/// continuous editor thumbnail atlas — only the nodes the canvas can currently
/// show, so a collapsed group or off-scope subgraph costs nothing.
#[derive(Debug, Clone)]
pub enum DumpRequest {
    /// Dump every node output of `EffectId` (Cmd+D → disk).
    All(EffectId),
    /// Dump only these nodes of `EffectId` (editor atlas → on-canvas thumbnails).
    Visible(EffectId, Vec<NodeId>),
}

impl DumpRequest {
    /// The effect whose chain this request targets — both variants carry it.
    pub fn effect_id(&self) -> &EffectId {
        match self {
            DumpRequest::All(eid) | DumpRequest::Visible(eid, _) => eid,
        }
    }
}

/// One dumped `Array` (storage-buffer) output for inspection: identity, the
/// live buffer, the per-item byte stride, and the channel layout as
/// `(name, kind, byte_offset)` where `kind` ∈ {`f32`,`i32`,`u32`,`vec2f`,
/// `vec3f`,`vec4f`}. The reader decodes the buffer against these fields.
pub struct ArrayDump<'a> {
    pub name: String,
    pub port: String,
    pub type_id: String,
    pub buffer: &'a manifold_gpu::GpuBuffer,
    pub item_size: u32,
    pub fields: Vec<(String, &'static str, u32)>,
}

/// Trait for compositing layers into a final output.
pub trait Compositor: Send {
    /// Render into the compositor's internal render targets.
    /// Returns the tonemapped output texture.
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        frame: &CompositorFrame,
    ) -> &manifold_gpu::GpuTexture;

    /// Resize compositor render targets.
    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32);

    /// Get current output dimensions.
    fn dimensions(&self) -> (u32, u32);

    /// Pre-tonemap HDR output texture.
    fn pre_tonemap_output(&self) -> &manifold_gpu::GpuTexture;

    /// The final compositor output texture (post-tonemap, post-effects).
    fn output_texture(&self) -> &manifold_gpu::GpuTexture;

    /// §24 5c with-effects clip thumbnails: the post-effect output texture for
    /// `clip_id` when it is the SOLE clip on its layer (so the layer output is that
    /// clip's full look — generator/video + layer effects). `None` for multi-clip
    /// layers (a clip can't be isolated) and for compositors without per-layer
    /// effects; the caller then falls back to the raw clip texture. Valid only for
    /// the frame just rendered. Default `None`.
    fn clip_post_fx_texture(&self, _clip_id: &str) -> Option<&manifold_gpu::GpuTexture> {
        None
    }

    /// Set (or clear) the authoring-time node-output preview request:
    /// `(watched effect, optional selected node)`. The chain holding the
    /// watched effect preserves the selected node's output texture for the
    /// editor to sample. Default no-op for compositors without effect chains.
    fn set_preview_request(&mut self, _request: Option<(EffectId, Option<NodeId>)>) {}

    /// The captured preview texture from the most recent `render`, if a
    /// preview is active and the watched node produced one. Default `None`.
    fn preview_texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        None
    }

    /// How the previewed node's output should be rendered in the editor preview
    /// (flow wheel for a vector field, lift for a scalar, raw for colour).
    /// Default `Color`.
    fn preview_encoding(&self) -> crate::node_graph::PreviewEncoding {
        crate::node_graph::PreviewEncoding::Color
    }

    /// Live scalar input / output values of the previewed node this frame, when
    /// it has no texture output — the data behind the editor's value inspector.
    /// Default empty.
    fn preview_scalar_io(&self) -> crate::node_graph::PreviewScalarIo {
        (Vec::new(), Vec::new())
    }

    /// Live (post-modulation) scalar param values for every node of the watched
    /// effect this frame, keyed by stable [`NodeId`] — so the editor canvas can
    /// show values that move under a card slider / driver / Ableton / envelope,
    /// not the frozen authoring def. Walks the watched chain set internally
    /// (the watched effect id comes from the active preview request). Default
    /// empty for compositors without effect chains.
    fn live_node_params(&self) -> crate::node_graph::LiveNodeParams {
        Vec::new()
    }

    /// Request a dump (whole-graph Cmd+D or visible-only atlas) on the next
    /// `render`, or clear it with `None`. Default no-op. See
    /// [`DumpRequest`] and [`Self::dump_textures`].
    fn set_dump_request(&mut self, _request: Option<DumpRequest>) {}

    /// After a `render` with a dump requested, every captured Texture2D output
    /// of the watched effect as `(node_id, port, type_id, texture)`. Default
    /// empty.
    fn dump_textures(&self) -> Vec<DumpTextureRef<'_>> {
        Vec::new()
    }

    /// Captured `Array` (storage-buffer) outputs of the watched effect after a
    /// dump `render`. Default empty.
    fn dump_arrays(&self) -> Vec<ArrayDump<'_>> {
        Vec::new()
    }

    /// Clean up per-owner effect state for a stopped clip.
    fn cleanup_clip_owner(&mut self, clip_id: &str);

    /// Clear all temporal effect state (e.g., on export warmup re-seek).
    fn clear_all_effect_state(&mut self);

    /// Flush in-flight background work in all effect processors.
    fn flush_all_background_work(&mut self);

    /// LED tap texture: pre-tonemap composite captured when led_exit_index == 0.
    /// Returns None if exit index is -1.
    fn led_tap_texture(&self) -> Option<&manifold_gpu::GpuTexture>;

    /// Per-layer LED composite texture: final post-tonemap + post-master-FX LED
    /// output built from layers flagged with `blit_to_led`. Returns None when no
    /// layers have blit_to_led enabled (fall back to led_tap_texture or output).
    fn led_composite_texture(&self) -> Option<&manifold_gpu::GpuTexture>;

    /// Snapshot of a specific effect type's internal graph, identified
    /// by its `PresetTypeId`. Default `None` — non-graph compositors
    /// have nothing to expose. The real `LayerCompositor` override
    /// delegates into its `EffectRegistry`.
    fn graph_snapshot_for(
        &self,
        _type_id: &manifold_core::PresetTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        None
    }

    /// Outer→inner routings for a specific effect type. Used by the
    /// per-card snapshot path where the snapshot is built off a
    /// serialized graph, so the editor still needs the static
    /// routing info from the live effect to disable inner rows.
    /// Default empty.
    fn outer_routings_for(
        &self,
        _type_id: &manifold_core::PresetTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        Vec::new()
    }

    /// Enable/disable per-step GPU attribution profiling on every effect
    /// chain (screen + LED + master) this compositor owns
    /// (PERF_BUDGET_GATE_DESIGN P2 / D6). Fans out to each chain's executor
    /// with its instance-identity scope (`fx:{layer_id}`, `master`,
    /// `led:{...}`) already applied at chain-insertion time. Default no-op
    /// for compositors without effect chains.
    fn set_profiling(&mut self, _on: bool) {}

    /// Force the serial composite path even with 2+ active layers
    /// (PERF_BUDGET_GATE_DESIGN D6 correction): profiled mode needs one
    /// shared compositor command buffer to attach the dispatch-profiling
    /// sampler to, and `composite_parallel` gives each layer its own command
    /// buffer. Default no-op for compositors without a parallel path.
    fn set_force_serial(&mut self, _on: bool) {}

    /// Drain every owned chain's per-step CPU profiles recorded on the last
    /// profiled frame (PERF_BUDGET_GATE_DESIGN P2). Default empty.
    fn take_step_profiles(&mut self) -> Vec<crate::node_graph::StepProfile> {
        Vec::new()
    }
}
