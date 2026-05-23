//! The [`EffectNode`] trait — the contract every effect, generator, primitive,
//! composite preset, and user-saved composite implements.

use ahash::AHashMap;
use manifold_core::{Beats, Seconds};

use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput};
use crate::node_graph::state_store::{OwnerKey, StateStore};

/// Stable string ID identifying an [`EffectNode`] kind.
///
/// Examples: `"node.blur"`, `"effect.bloom"`, `"composite.user.<uuid>"`.
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
    /// Monotonic frame counter from the host. Effects that throttle
    /// expensive work (e.g. depth inference / blob detection /
    /// wireframe mesh rebuild every N frames) gate on the delta of
    /// this counter. Zero means "host didn't supply" — tests should
    /// pass 0 unless they specifically exercise throttling logic.
    pub frame_count: i64,
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
    /// Persistent state store for stateful nodes. `None` for stateless
    /// graphs and tests that don't exercise `NodeState`. Identity for
    /// keying is provided by `node_id` + `owner_key` below.
    pub state: Option<&'ctx mut StateStore>,
    /// Identity of the node currently evaluating. Set by the executor
    /// at each step; nodes use it to key their state buckets.
    pub node_id: NodeInstanceId,
    /// Owner identity for state keying — `0` for master, `layer_index +
    /// 1` for a layer, `hash(clip_id)` for a clip. Matches the legacy
    /// `EffectContext::owner_key` namespace.
    pub owner_key: OwnerKey,
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
            state: None,
            node_id: NodeInstanceId(0),
            owner_key: 0,
        }
    }

    /// Constructor with state plumbing. Used by the executor's stateful
    /// execution path; tests and stateless graphs use [`new`] above.
    #[allow(clippy::too_many_arguments)]
    pub fn with_state(
        time: FrameTime,
        params: &'ctx ParamValues,
        inputs: NodeInputs<'ctx>,
        outputs: NodeOutputs<'ctx>,
        gpu: Option<&'ctx mut crate::gpu_encoder::GpuEncoder<'gpu>>,
        state: Option<&'ctx mut StateStore>,
        node_id: NodeInstanceId,
        owner_key: OwnerKey,
    ) -> Self {
        Self {
            time,
            params,
            inputs,
            outputs,
            gpu,
            state,
            node_id,
            owner_key,
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

    /// Borrow the [`StateStore`], panicking if absent. Use the node's
    /// `node_id` + `owner_key` (also on this ctx) as the key when
    /// inserting / fetching typed state.
    pub fn state_store(&mut self) -> &mut StateStore {
        self.state
            .as_deref_mut()
            .expect("EffectNodeContext::state_store called without a StateStore bound")
    }

    /// Resolve a scalar value via the port-shadows-param convention:
    ///
    /// 1. If a scalar wire is connected to the input port named
    ///    `name`, return its current `f32` value.
    /// 2. Else, if the node has a param named `name` of type Float,
    ///    return that.
    /// 3. Else return `default`.
    ///
    /// The canonical helper for any primitive whose input port and
    /// param share a name (`freq_x`, `phase`, `scale`, `amount`, …).
    /// Before this helper existed eight primitives carried local
    /// `fn read_scalar` copies; centralizing here removes the
    /// duplication and lets future primitives just call
    /// `ctx.scalar_or_param("name", default)`.
    pub fn scalar_or_param(&self, name: &str, default: f32) -> f32 {
        match self.inputs.scalar(name) {
            Some(crate::node_graph::parameters::ParamValue::Float(f)) => f,
            _ => match self.params.get(name) {
                Some(crate::node_graph::parameters::ParamValue::Float(f)) => *f,
                _ => default,
            },
        }
    }
}

/// Runtime services a node may require from the executor during
/// `evaluate`. Set by overriding [`EffectNode::requires`]; aggregated
/// across the graph at [`compile`](crate::node_graph::compile) time
/// and checked at the executor entry point before any node fires.
///
/// Catches the "node needs X but the executor entry didn't provide
/// X" mismatch at the boundary instead of mid-evaluate via an
/// `.expect()` panic. Today's surface:
///   - `state_store` — the node calls
///     [`EffectNodeContext::state_store`] (today only
///     [`Feedback`](crate::node_graph::primitives::Feedback)).
///   - `gpu_encoder` — the node calls
///     [`EffectNodeContext::gpu_encoder`] (every real-GPU primitive).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeRequires {
    pub state_store: bool,
    pub gpu_encoder: bool,
}

impl NodeRequires {
    /// Union with another `NodeRequires`. Used by `compile()` to roll
    /// up the per-node declarations into a single per-plan summary.
    pub fn union(self, other: Self) -> Self {
        Self {
            state_store: self.state_store || other.state_store,
            gpu_encoder: self.gpu_encoder || other.gpu_encoder,
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
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>);

    /// Static declaration of the `(input_port, output_port)` pair
    /// this node would alias when `skip_passthrough` fires. Must not
    /// depend on params, frame state, or anything dynamic.
    ///
    /// The slot lifetime planner reads this at `compile()` time and
    /// extends `R(input_port)`'s lifetime to cover every reader of
    /// `R(output_port)`. Without that extension, a downstream step
    /// could recycle `R(input_port)`'s physical slot before all
    /// alias readers complete — the alias holds a clone of the
    /// underlying `MTLTexture`, so a later write into that slot
    /// would silently corrupt downstream reads through the alias.
    /// In linear chains today the bug is latent; in fan-out
    /// topologies (V2 user composites, multi-output sub-graphs) it
    /// would manifest.
    ///
    /// Default: `None` (node never skips). A node overriding
    /// [`skip_passthrough`] to return `Some(...)` for any params
    /// MUST also override this to declare the same port pair —
    /// the planner uses this declaration, the runtime uses
    /// `skip_passthrough`. They must agree.
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Optional skip-passthrough hook: if the node is a no-op for the
    /// current frame (e.g. an `amount`-style param is zero), return
    /// `Some((input_port, output_port))` indicating which input the
    /// runtime should alias onto which output. The runtime then installs
    /// the input slot's texture into the output slot as a transient
    /// borrowed override — **zero GPU work** — and skips `evaluate`
    /// entirely.
    ///
    /// Default: `None` (always run `evaluate`).
    ///
    /// Equivalent to the legacy chain dispatch's "skip + don't swap"
    /// semantic: downstream effects see the upstream's content as if
    /// this node were transparent. Without this hook, a skipping node
    /// would leave its dedicated output slot frozen at whatever was
    /// last written there, which downstream effects would read as
    /// stale data (the classic Quad-Mirror-at-amount=0 +
    /// Stylized-Feedback runaway from 2026-05-13).
    ///
    /// Implementors must ensure the returned port pair maps to slots
    /// of compatible type — currently only `Texture2D` is supported.
    /// Aliases auto-clear at the start of each frame so a non-skip
    /// frame's real write isn't shadowed.
    ///
    /// **Must match [`skip_passthrough_ports`]'s declaration** —
    /// the runtime uses this for the dynamic decision, the planner
    /// uses `skip_passthrough_ports` for static lifetime extension.
    /// Returning a different port pair than declared is a programmer
    /// error and may corrupt downstream reads in fan-out topologies.
    fn skip_passthrough(&self, _params: &ParamValues) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Runtime services this node's `evaluate` requires from the
    /// executor. The compiler aggregates these across all nodes in
    /// the graph and the executor's entry points validate against
    /// the aggregate at the boundary — so a graph containing
    /// [`Feedback`](crate::node_graph::primitives::Feedback) dispatched
    /// via `execute_frame_with_gpu` (no state) panics with a clean
    /// message *before* GPU work starts, instead of mid-frame inside
    /// the primitive's `state_store().expect(...)`.
    ///
    /// Default: empty (no requirements). Nodes that call
    /// [`EffectNodeContext::state_store`] or
    /// [`EffectNodeContext::gpu_encoder`] inside their `evaluate`
    /// must override this to declare the corresponding requirement.
    fn requires(&self) -> NodeRequires {
        NodeRequires::default()
    }

    /// Declared `(input_port, output_port)` pairs that share a single
    /// physical buffer. Used by stateful array simulators
    /// (`integrate_particles`, `integrate_particles_attractor`) where
    /// the GPU dispatch reads from and writes to the same storage in
    /// place. The chain builder pre-allocates one buffer per pair —
    /// sized by the input wire's capacity — and aliases the output's
    /// slot to the input's, so upstream writes flow through and
    /// cross-frame state lives in the chain-allocated buffer. Default:
    /// empty (no aliasing).
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Reset persistent state (previous-frame textures, accumulators,
    /// density grids, mip pyramids, StateStore entries — anything the
    /// node holds across frames). Default: no-op for stateless nodes.
    ///
    /// **If your node has any persistent state, you MUST override this.**
    /// The compositor fires `clear_state` on every chain whose layer
    /// went idle this frame (no active clips, muted, soloed-out) — a
    /// non-overriding stateful node will accumulate feedback indefinitely
    /// across mute/unmute cycles, producing the classic "feedback runs
    /// away to saturation" bug. It's also fired on seek and project
    /// load. Override it to drop every persistent texture / buffer the
    /// node owns (typically `self.feedback = None;` etc.) — next
    /// `evaluate` re-allocates and re-clears as needed.
    ///
    /// See `docs/EFFECT_CHAIN_LIFECYCLE.md` for the full lifecycle
    /// and the symptom → cause table when feedback effects misbehave.
    fn clear_state(&mut self) {}

    /// Optional WGSL kernel source for the WGSL-escape-hatch primitive
    /// family (`node.wgsl_compute_*`). Returns the source string the
    /// node was constructed with; `None` for every node whose shader
    /// is fixed at compile time via `include_str!`.
    ///
    /// Persistence calls this on `from_graph` to write the kernel into
    /// the saved JSON, alongside the other per-node fields like
    /// `editor_pos`. Not a parameter — agents/users can't drive it
    /// from an LFO, expose it on the outer card, or modulate it.
    /// It's identity-level config of the node, set once.
    fn wgsl_source(&self) -> Option<&str> {
        None
    }

    /// Optional setter for [`wgsl_source`](Self::wgsl_source). Called
    /// by `into_graph` after `new()` constructs the node so the kernel
    /// is in place before the first `evaluate`. No-op for every node
    /// whose shader is fixed at compile time.
    fn set_wgsl_source(&mut self, _source: &str) {}

    /// Output texture format for the named port. Returns `None` to use
    /// the backend's default format (typically `Rgba16Float`). Only
    /// meaningful for `Texture2D` outputs — other port types ignore
    /// the value.
    ///
    /// The graph runtime queries this at `compile()` time to record
    /// each resource's format alongside its `PortType`. The backend
    /// then allocates per-slot at the producer's declared format and
    /// keys slot recycling on `(PortType, GpuTextureFormat)` so two
    /// slots with different formats never alias.
    ///
    /// Most primitives default to `None` (use the backend's format,
    /// which is correct for color/video). Native-precision escape
    /// hatches (`node.wgsl_compute_*` with format override) and any
    /// stateful primitive that needs `r32float` / `rgba32float`
    /// stability override this.
    fn output_format(&self, _port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        None
    }

    /// Optional setter for [`output_format`](Self::output_format).
    /// Called by `into_graph` after `new()` so a JSON-declared format
    /// override is in place before `compile()` walks outputs. No-op
    /// for every node whose format is fixed at compile time.
    fn set_output_format(&mut self, _port: &str, _format: manifold_gpu::GpuTextureFormat) {}

    /// How many items to pre-allocate for the named `Array<T>` output
    /// port. The chain build / JsonGraphGenerator pre-allocator calls
    /// this once after the node's params are set and all its input
    /// Array buffers are bound; the result drives a single
    /// `(item_size × capacity)`-byte buffer allocation.
    ///
    /// **Three canonical patterns** map onto this single method, all
    /// implementable on the producer node itself:
    ///
    /// 1. **Producer** — capacity is fixed by a node-local param. The
    ///    default impl reads `params["max_capacity"]` and returns its
    ///    integer value. Generator-style primitives
    ///    (`seed_particles`, `generate_lissajous`, …) match this
    ///    pattern without an override.
    ///
    /// 2. **Transform (same-as-input)** — capacity matches a named
    ///    input port's bound buffer count. `integrate_particles`,
    ///    `rotate_3d`, `project_4d` etc. override this and return
    ///    `input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)`.
    ///    The pre-allocator hands the bound capacities in via the
    ///    `input_capacities` slice — one entry per Array input that
    ///    was successfully pre-bound earlier in the plan walk.
    ///
    /// 3. **Computed-from-params** — capacity is a function of
    ///    multiple params (e.g. `scatter_particles`' `width × height`,
    ///    `triangulate_grid`'s `(src_cols-1) × (src_rows-1) × 6`).
    ///    Override and compute from `params` directly.
    ///
    /// Returning `None` for an Array output declares "I can't tell you
    /// the capacity right now" — the pre-allocator emits a `log::warn!`
    /// and skips the allocation. Downstream consumers see an empty
    /// wire and warn at draw time. Almost always a bug.
    ///
    /// A registry-wide test
    /// (`every_array_output_declares_a_valid_capacity_source`) walks
    /// every registered primitive's outputs and asserts every Array
    /// output's declared capacity is resolvable from default params /
    /// default input shape — catches "primitive declares Array output
    /// but forgot to teach the pre-allocator how to size it" at CI
    /// time.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let is_array_output = self
            .outputs()
            .iter()
            .any(|p| p.name == port_name && matches!(p.ty, crate::node_graph::ports::PortType::Array(_)));
        if !is_array_output {
            return None;
        }
        params
            .get("max_capacity")
            .and_then(|v| v.as_u32_clamped(1))
    }
}
