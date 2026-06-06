use crate::gpu_encoder::GpuEncoder;
use crate::preset_context::PresetContext;
use manifold_core::PresetTypeId;
use std::any::Any;
use std::collections::BTreeMap;

/// GPU-aware generator processor. Each instance owns its manifold-gpu pipeline(s)
/// and any per-generator GPU state (compute buffers, temporal state, etc.).
///
/// Lifecycle:
/// - `new()` creates the instance and compiles all pipelines
/// - `render()` is called once per frame per active clip of this type
/// - `resize()` recreates any resolution-dependent resources
/// - Drop cleans up GPU resources automatically
pub trait Generator: Send {
    /// Which generator type this handles.
    fn generator_type(&self) -> &PresetTypeId;

    /// Render one frame into the target texture.
    /// Returns updated anim_progress for this clip.
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &PresetContext,
    ) -> f32;

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32);

    /// Reset all simulation state to initial conditions.
    /// Called after export warmup re-seek to avoid stale particle/density state.
    /// Default: no-op (stateless generators don't need this).
    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {}

    /// Provide per-clip string parameters (e.g. text content for a text generator).
    /// Called once per frame before `render()`. Only generators that need string
    /// data override this; all others inherit the no-op default.
    fn set_string_params(&mut self, _params: Option<&BTreeMap<String, String>>) {}

    /// Apply the host's per-instance reshape notes
    /// (`PresetInstance.param_mappings`) to this generator's bindings.
    /// Called once per frame before `render()` with the layer's current
    /// notes + their version. The default is a no-op (Rust generators and
    /// note-free graphs ignore it); `JsonGraphGenerator` overrides it to
    /// rebuild the affected binding reshapes + clear its apply-cache, but
    /// only when `version` advances past the last seen one — so a note-free
    /// generator pays a single integer compare per frame. The reshape is a
    /// downstream override at the render boundary; it never touches the
    /// value slot the modulation surface writes.
    fn apply_param_notes(
        &mut self,
        _notes: &[manifold_core::effects::ParamMapping],
        _version: u32,
    ) {
    }

    /// Downcast hook. Default impl is sufficient for any concrete
    /// generator that implements `Generator` directly. Mirrors the
    /// `ClipRenderer::as_any` pattern — used by regression tests that
    /// need to introspect a specific generator implementation's
    /// internal state (e.g. confirm a `JsonGraphGenerator`'s backend
    /// reports the expected canvas dims after a host-driven rebuild).
    fn as_any(&self) -> &dyn Any {
        unimplemented!(
            "Generator::as_any must be overridden by the concrete type to enable downcasting"
        )
    }

    /// Aim the authoring-time output preview at `node_id`, or clear it.
    /// Default no-op (Rust generators have no inner graph to preview);
    /// `JsonGraphGenerator` overrides it to preserve that node's output.
    fn set_preview_node(&mut self, _node_id: Option<&manifold_core::NodeId>) {}

    /// The preview target's captured output texture from the most recent
    /// `render`, if a node is targeted and produced one. Default `None`.
    fn preview_texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        None
    }

    /// How the currently-previewed node's output should be rendered in the
    /// editor preview (flow wheel for a vector field, lift for a scalar, raw
    /// for colour). Default `Color`. Set when [`Self::set_preview_node`] resolves
    /// a target.
    fn preview_encoding(&self) -> crate::node_graph::PreviewEncoding {
        crate::node_graph::PreviewEncoding::Color
    }

    /// Live scalar input / output values on the previewed node this frame
    /// (`port_name`, value), when it has no texture output — the data behind
    /// the editor's value inspector. Default empty.
    fn preview_scalar_io(&self) -> crate::node_graph::PreviewScalarIo {
        (Vec::new(), Vec::new())
    }

    /// Push the override def's inner-node param values into the live runtime
    /// graph without rebuilding it — the in-place path for a value-only graph
    /// edit, which bumps the layer's snapshot `generator_graph_version` but not
    /// its `generator_graph_structure_version`, so the generator wasn't
    /// re-instantiated. Default no-op (Rust generators have no inner graph);
    /// `JsonGraphGenerator` overrides it so a slider tweak in the editor lands
    /// without resetting the generator's sim/particle state.
    fn apply_inner_param_overrides(
        &mut self,
        _def: &manifold_core::effect_graph_def::EffectGraphDef,
    ) {
    }
}
