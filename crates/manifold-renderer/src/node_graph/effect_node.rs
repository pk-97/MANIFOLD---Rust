//! The [`EffectNode`] trait — the contract every effect, generator, primitive,
//! composite preset, and user-saved composite implements.

use ahash::AHashMap;
use manifold_core::{Beats, Seconds};

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
/// The exact shape of input/output bindings is defined by the graph runtime
/// (subsequent step). For now the context carries only the minimum needed for
/// the trait to compile; future runtime work will populate it with concrete
/// resource handles (GPU encoder, input/output texture bindings, etc.).
pub struct EffectNodeContext<'a> {
    pub time: FrameTime,
    pub params: &'a ParamValues,
    // gpu encoder, input bindings, output bindings — added in step 2.
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> EffectNodeContext<'a> {
    pub fn new(time: FrameTime, params: &'a ParamValues) -> Self {
        Self {
            time,
            params,
            _phantom: std::marker::PhantomData,
        }
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
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_>);

    /// Reset persistent state (previous-frame textures, accumulators, density
    /// grids). Called on seek so trails and feedback don't carry stale content.
    /// Default: no-op for stateless nodes.
    fn clear_state(&mut self) {}
}
