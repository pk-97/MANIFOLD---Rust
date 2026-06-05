use crate::gpu_encoder::GpuEncoder;
use crate::layer_compositor::CompositeClipDescriptor;
use crate::tonemap::TonemapSettings;
use manifold_core::BlendMode;
use manifold_core::LayerId;
use manifold_core::effects::{EffectGroup, EffectInstance};
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
    pub effects: &'a [EffectInstance],
    pub effect_groups: &'a [EffectGroup],
    /// Parent group layer ID (None for root layers).
    pub parent_layer_id: Option<&'a LayerId>,
    /// Whether this layer is a group container.
    pub is_group: bool,
}

/// Frame context passed to the compositor each tick.
pub struct CompositorFrame<'a> {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    pub frame_count: u64,
    pub compositor_dirty: bool,
    pub clips: &'a [CompositeClipDescriptor<'a>],
    pub layers: &'a [CompositeLayerDescriptor<'a>],
    pub master_effects: &'a [EffectInstance],
    pub master_effect_groups: &'a [EffectGroup],
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
}

/// One dumped texture borrowed from a compositor: `(node_id, port, type_id,
/// texture)`. The strings are owned; the texture borrows the compositor.
pub type DumpTextureRef<'a> = (String, String, String, &'a manifold_gpu::GpuTexture);

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

    /// Request a one-shot "dump every output" of effect `effect_id` on the next
    /// `render`, or clear it. Default no-op. See [`Self::dump_textures`].
    fn set_dump_request(&mut self, _effect_id: Option<EffectId>) {}

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
    /// by its `EffectTypeId`. Default `None` — non-graph compositors
    /// have nothing to expose. The real `LayerCompositor` override
    /// delegates into its `EffectRegistry`.
    fn graph_snapshot_for(
        &self,
        _type_id: &manifold_core::EffectTypeId,
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
        _type_id: &manifold_core::EffectTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        Vec::new()
    }
}
