//! The [`EffectNode`] trait — the contract every effect, generator, primitive,
//! composite preset, and user-saved composite implements.

use ahash::AHashMap;
use manifold_core::{Beats, Seconds};

use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput};

/// Stable string ID identifying an [`EffectNode`] kind.
///
/// Examples: `"primitive.blur"`, `"effect.bloom"`, `"composite.user.<uuid>"`.
///
/// Treated as public API once shipped to users. Renaming in place breaks
/// project files. Use additive deprecation (introduce `effect.bloom_v2`,
/// keep `effect.bloom` working) instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectNodeType(pub String);

impl EffectNodeType {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Unique runtime identity of one node instance within a graph.
///
/// State for stateful nodes (Feedback, Bloom mip chain, FluidSim density grid)
/// is keyed by this. Stable for the lifetime of the graph instance — even
/// across topology edits in V2 — so persistent state survives rewiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInstanceId(pub u32);

/// Directed connection from one [`EffectNode`]'s output to another's input.
#[derive(Debug, Clone)]
pub struct NodeWire {
    /// `(source node, output port name)`.
    pub from: (NodeInstanceId, &'static str),
    /// `(target node, input port name)`.
    pub to: (NodeInstanceId, &'static str),
}

/// Per-frame timing supplied to every [`EffectNode::evaluate`] call.
#[derive(Debug, Clone, Copy)]
pub struct FrameTime {
    pub beats: Beats,
    pub seconds: Seconds,
    pub delta: Seconds,
}

/// Map of parameter name → current value for one node instance, one frame.
pub type ParamValues = AHashMap<&'static str, ParamValue>;

/// What an [`EffectNode`] sees during `evaluate`.
///
/// The runtime populates this each step with the bindings produced by the
/// execution plan: which slot supplies each input, which slot receives each
/// output, and (for nodes that issue real GPU work) the per-frame
/// [`GpuEncoder`] borrowed from the host.
///
/// The `gpu` field is `None` for tests against [`MockBackend`] — those
/// exercise the runtime's resource lifetime / acquire-release logic without
/// needing a real Metal command buffer. Production paths (with
/// [`MetalBackend`]) thread a real encoder through and any node that
/// dispatches compute / encodes a render pass should `expect()` it.
///
/// Two lifetimes:
///   - `'ctx` — borrow scope of one `evaluate` call (params, slot bindings,
///     and the outer encoder reference).
///   - `'gpu` — internal lifetime of the [`GpuEncoder`]'s wrapped Metal
///     command buffer / device. Lives longer than `'ctx`.
///
/// [`GpuEncoder`]: crate::gpu_encoder::GpuEncoder
/// [`MockBackend`]: crate::node_graph::MockBackend
/// [`MetalBackend`]: crate::node_graph::MetalBackend
pub struct EffectNodeContext<'ctx, 'gpu> {
    pub time: FrameTime,
    pub params: &'ctx ParamValues,
    pub inputs: NodeInputs<'ctx>,
    pub outputs: NodeOutputs<'ctx>,
    pub gpu: Option<&'ctx mut crate::gpu_encoder::GpuEncoder<'gpu>>,
}

impl<'ctx, 'gpu> EffectNodeContext<'ctx, 'gpu> {
    pub fn new(
        time: FrameTime,
        params: &'ctx ParamValues,
        inputs: NodeInputs<'ctx>,
        outputs: NodeOutputs<'ctx>,
        gpu: Option<&'ctx mut crate::gpu_encoder::GpuEncoder<'gpu>>,
    ) -> Self {
        Self {
            time,
            params,
            inputs,
            outputs,
            gpu,
        }
    }

    /// Borrow the [`GpuEncoder`], panicking if absent.
    ///
    /// Use this in real-GPU node implementations after asserting (in their
    /// docs / contract) that they require a backend-backed executor.
    /// Mock-backend tests never call into real-GPU evaluate paths so the
    /// panic should be unreachable in correctly-typed code.
    pub fn gpu_encoder(&mut self) -> &mut crate::gpu_encoder::GpuEncoder<'gpu> {
        self.gpu
            .as_deref_mut()
            .expect("EffectNodeContext::gpu_encoder called without a GpuEncoder bound")
    }
}

/// One unit of GPU work in the effect graph.
///
/// Implemented by:
///  - **Primitives** (Blur, Threshold, Mix, …) — small reusable building blocks.
///  - **Atomic effects/generators** (FluidSim, Plasma, Glitch) — irreducibly
///    one thing, opaque internals, possibly multiple outputs.
///  - **Composite presets** (Bloom, Halation, …) — implemented as a sub-graph
///    of other `EffectNode`s. The graph engine does not distinguish atomic
///    from composite; both implement this trait.
///  - **Boundary nodes** ([`Source`], [`FinalOutput`]) — graph-level placeholders
///    representing data entering/leaving the graph.
///
/// V1 graphs have at most one `Source` (Texture2D) and exactly one
/// `FinalOutput` (Texture2D). Multi-input/multi-output composites are deferred
/// to a later phase. Atomic implementations are free to declare multiple
/// outputs (FluidSim's `density`/`velocity`, BlackHole's `lens_field`) — those
/// are part of the atomic node's port shape, not the graph's boundary.
///
/// [`Source`]: # "Boundary node — input edge of the graph."
/// [`FinalOutput`]: # "Boundary node — output edge of the graph."
pub trait EffectNode: Send {
    /// Stable type ID. See [`EffectNodeType`] for the renaming policy.
    fn type_id(&self) -> &EffectNodeType;

    /// Static input ports — name, type, optionality. Same shape every call;
    /// queried at graph compile time, not per frame.
    fn inputs(&self) -> &[NodeInput];

    /// Static output ports — name and type. Same shape every call.
    fn outputs(&self) -> &[NodeOutput];

    /// Static parameter definitions — name, type, default, range.
    fn parameters(&self) -> &[ParamDef];

    /// Run one frame of GPU work: read inputs, write outputs.
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>);

    /// Reset persistent state (previous-frame textures, accumulators, density
    /// grids). Called on seek so trails and feedback don't carry stale content.
    /// Default: no-op for stateless nodes.
    fn clear_state(&mut self) {}
}
