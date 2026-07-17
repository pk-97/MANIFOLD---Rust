//! The [`EffectNode`] trait — the contract every effect, generator, primitive,
//! composite preset, and user-saved composite implements.

use ahash::AHashMap;
use manifold_core::{Beats, Seconds};

use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
use crate::node_graph::material::MaterialKind;
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
///
/// Keyed by `Cow<'static, str>` so variadic nodes (`render_scene`) can store
/// values under an owned, formatted param name (`pos_x_37`) with no object
/// cap, while every fixed-param node keeps borrowing a `&'static str` key
/// (`Cow::Borrowed`) at zero cost — including the per-frame `set_param_unchecked`
/// hot path. Lookups take `&str` via `Borrow`, so callers are unaffected.
pub type ParamValues = AHashMap<std::borrow::Cow<'static, str>, ParamValue>;

/// Bridge a (possibly-owned) port/param name to a `&'static str`.
///
/// `NodePort`/`ParamDef` names are `Cow<'static, str>` so variadic nodes can
/// format per-instance names (`mesh_37`) with no object cap. But the execution
/// plan, the freeze compiler, and the wire graph key everything by
/// `&'static str` — and the per-frame executor reads those names, so keeping
/// them `&'static` avoids any hot-path allocation (Peter's Option-B call,
/// 2026-07-05). This bridges the two: a `Borrowed` name (every fixed port —
/// the overwhelming majority) returns its `&'static str` for free; an `Owned`
/// name (a variadic node's formatted port) is interned once into a
/// process-lifetime set and reused thereafter.
///
/// The intern set is the bounded-leak pattern already used for wire handles in
/// `graph_loader` (which leaks JSON-derived `&'static` names): dynamic names
/// are few, short, and repetitive (`mesh_0..N`, `pos_x_0..N`), so the set
/// saturates almost immediately and never grows on the per-frame path — every
/// call here is at plan-build / graph-edit time, never inside `evaluate`.
/// Deduped, so re-interning the same name returns the same pointer and the
/// leak stays bounded across repeated plan rebuilds.
// The `&Cow` argument is load-bearing, NOT the `ptr_arg` anti-pattern clippy
// flags: this function branches on `Borrowed` vs `Owned` to return a fixed
// port's `&'static str` for free (zero leak) and only intern genuinely owned
// (variadic) names. A `&str` arg would erase that distinction and force every
// name — including the static majority — through the intern/leak path. Remove
// this allow only if `intern_name` stops needing the borrowed/owned split.
#[allow(clippy::ptr_arg)]
pub fn intern_name(name: &std::borrow::Cow<'static, str>) -> &'static str {
    use ahash::AHashSet;
    use std::sync::{Mutex, OnceLock};
    match name {
        // Fixed ports: already `'static`, no leak, no lock.
        std::borrow::Cow::Borrowed(s) => s,
        // Variadic ports: intern once, reuse forever.
        std::borrow::Cow::Owned(s) => {
            static INTERN: OnceLock<Mutex<AHashSet<&'static str>>> = OnceLock::new();
            let set = INTERN.get_or_init(|| Mutex::new(AHashSet::new()));
            let mut guard = set.lock().expect("name intern set poisoned");
            if let Some(&existing) = guard.get(s.as_str()) {
                return existing;
            }
            let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
            guard.insert(leaked);
            leaked
        }
    }
}

/// One conditional input-requirement rule on an [`EffectNode`].
///
/// Declared by nodes whose required-input set varies with an upstream-wire
/// value — currently the 3D mesh renderers, whose required inputs depend
/// on the wired [`Material`](crate::node_graph::material::Material)'s
/// [`MaterialKind`]: e.g. PBR requires `light` AND `envmap`, Unlit
/// requires neither.
///
/// The validator reads this list at preset-load time and (for each node
/// with non-empty rules) walks the wired material's source primitive,
/// reads its [`EffectNode::emitted_material_kind`], picks the matching
/// rule, and checks every entry in `required_inputs` has a wire — raising
/// [`GraphError::ConditionalRequirementUnmet`](crate::node_graph::validation::GraphError::ConditionalRequirementUnmet)
/// when an input is missing.
///
/// The runtime catches the dynamic case (material flows through a mux,
/// material's source isn't a registered atom) via [`EffectNodeContext::error`].
#[derive(Debug, Clone, Copy)]
pub struct ConditionalRequirement {
    /// Which material kind triggers this rule.
    pub on_material_kind: MaterialKind,
    /// Input port names that must be wired when the rule fires.
    pub required_inputs: &'static [&'static str],
}

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
    /// `PresetContext::owner_key` namespace.
    pub owner_key: OwnerKey,
    /// Did this primitive's `evaluate` / `run` access the GPU encoder?
    /// Set to `true` by [`gpu_encoder`](Self::gpu_encoder) on first
    /// call. The executor reads this after `evaluate` returns to verify
    /// the aliased-output contract — a primitive that declared
    /// `aliased_array_io` but never touched the GPU clearly didn't
    /// dispatch the kernel that's supposed to mutate the aliased
    /// buffer, leaving downstream consumers reading stale data. Debug
    /// builds panic loudly; release builds silently accept (the cost
    /// of a per-frame check on the hot path is the trade-off).
    pub gpu_accessed: bool,
    /// Per-step scratch into which a primitive pushes structured error
    /// messages via [`Self::error`]. The executor drains and logs them
    /// after `evaluate` returns, one line per error. Errors do NOT halt
    /// the frame — the primitive should also emit a deterministic
    /// fallback (e.g. magenta clear on a Texture2D output) so the rest
    /// of the graph isn't poisoned by garbage.
    ///
    /// `None` when the context is constructed via legacy paths that
    /// don't thread an error scratch — the `error` method is a silent
    /// no-op in that case (test-construction shortcut).
    pub errors: Option<&'ctx mut Vec<String>>,
    /// Zero-copy feedback ping-pong: a `late_capture` implementation
    /// sets this (via [`Self::request_texture_swap`]) to ask the
    /// executor to swap the textures bound at one of its OUTPUT ports
    /// and one of its INPUT ports — both must resolve to persistent
    /// slots. Read and performed by the executor after `late_capture`
    /// returns; ignored on the `evaluate` path.
    pub texture_swap_request: Option<(&'static str, &'static str)>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5/I3 — set by
    /// [`Self::mark_outputs_unchanged`], read by the executor after
    /// `run`/`evaluate` returns and stored per node per frame (P1 stub —
    /// nothing consumes the stored value yet; P2 gates dirty-caching
    /// decisions on it). `false` by default: a node that never calls the
    /// API is always treated as having produced fresh output this frame,
    /// which is the safe (never-stale) direction.
    pub outputs_unchanged: bool,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6 — the evaluating
    /// [`Executor`](crate::node_graph::execution::Executor)'s rebuild epoch:
    /// a process-global monotonic counter assigned once at `Executor::new()`
    /// (never changes for that executor's lifetime). Exists so a node that
    /// caches a dirty-check key derived from [`NodeInputs::slot_generation`]
    /// (e.g. `render_scene`'s shadow-map cache) can fold in "which executor
    /// lifetime this key was computed under" — a topology rebuild
    /// (`PresetRuntime::harvest_state_from`) can carry the node's own Rust
    /// state across into a BRAND NEW executor whose generation counters
    /// reset to 0, so a stale cached key must never coincidentally match the
    /// new executor's low generation numbers. `0` on the legacy [`Self::new`]
    /// constructor (no executor lifetime concept — test/standalone paths).
    pub rebuild_epoch: u64,
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
            gpu_accessed: false,
            errors: None,
            texture_swap_request: None,
            outputs_unchanged: false,
            rebuild_epoch: 0,
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
        rebuild_epoch: u64,
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
            gpu_accessed: false,
            errors: None,
            texture_swap_request: None,
            outputs_unchanged: false,
            rebuild_epoch,
        }
    }

    /// Zero-copy feedback ping-pong (called from `late_capture` only):
    /// ask the executor to swap the textures bound at `out_port` (one of
    /// this node's outputs) and `in_port` (one of its inputs). Both must
    /// resolve to PERSISTENT slots — the executor performs the swap via
    /// [`Backend::swap_texture_2d`](crate::node_graph::Backend::swap_texture_2d)
    /// after `late_capture` returns and ignores the request if either
    /// slot isn't bound.
    pub fn request_texture_swap(&mut self, out_port: &'static str, in_port: &'static str) {
        self.texture_swap_request = Some((out_port, in_port));
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5 — declare that this
    /// node's outputs this frame are unchanged from last frame.
    ///
    /// **I3's contract, verbatim: a node may declare it only when its
    /// outputs are bit-identical to the previous frame *including physical
    /// output identity*.** "Physical output identity" means the actual
    /// slot/buffer/texture object a downstream consumer would read from —
    /// not just the bytes. A node whose output slot was handed a
    /// different physical resource this frame (pool recycle, resize) must
    /// NOT call this even if the bytes it would have written are the same
    /// as last time's, because the resource that held those bytes is
    /// gone.
    ///
    /// Call this ONLY on the exact code path that skipped writing (the
    /// node's actual no-op branch) — never speculatively, never on a path
    /// that also performs a write. The default (never calling this) is
    /// always safe: it means "assume dirty", which is correct but leaves
    /// the waste this design removes. Declaring falsely is the only way
    /// to introduce staleness under this API — there is no other failure
    /// mode, so per-declaring-node parity tests (frame N readback ==
    /// fresh-executor frame 1 readback) are the enforcement (I3).
    pub fn mark_outputs_unchanged(&mut self) {
        self.outputs_unchanged = true;
    }

    /// Attach a per-step error scratch buffer. The executor calls this
    /// each step before invoking `evaluate` / `late_capture`. Primitives
    /// push structured errors into the buffer via [`Self::error`]; the
    /// executor drains and logs them after the primitive returns.
    pub fn with_errors(mut self, errors: &'ctx mut Vec<String>) -> Self {
        self.errors = Some(errors);
        self
    }

    /// Report a structured error for the current node. The executor
    /// drains errors after `evaluate` returns, logs each one once per
    /// invocation, and (in future versions) surfaces them to the
    /// editor's error toast.
    ///
    /// Use when an input is missing OR has a value the node can't
    /// process (e.g., a conditional requirement is unmet). Does NOT
    /// halt the frame — the node should ALSO emit a deterministic
    /// fallback (magenta clear on a Texture2D output, zero values on
    /// scalar outputs) so the rest of the graph isn't poisoned by
    /// garbage.
    ///
    /// A no-op when the context was constructed without an error
    /// scratch (legacy test paths). Production execution always wires
    /// one in.
    pub fn error(&mut self, message: impl Into<String>) {
        if let Some(buf) = self.errors.as_deref_mut() {
            buf.push(message.into());
        }
    }

    /// Borrow the [`GpuEncoder`], panicking if absent.
    ///
    /// Use this in real-GPU node implementations after asserting (in their
    /// docs / contract) that they require a backend-backed executor.
    /// Mock-backend tests never call into real-GPU evaluate paths so the
    /// panic should be unreachable in correctly-typed code.
    ///
    /// Side-effect: marks `gpu_accessed = true` so the executor's
    /// post-evaluate aliased-output contract check passes for any
    /// primitive that touched the GPU. Primitives that just want to
    /// inspect inputs / write scalar outputs without touching the GPU
    /// must not call this (they aren't subject to the aliased-output
    /// contract either, since they can't declare aliased_array_io
    /// meaningfully without dispatching).
    pub fn gpu_encoder(&mut self) -> &mut crate::gpu_encoder::GpuEncoder<'gpu> {
        self.gpu_accessed = true;
        self.gpu
            .as_deref_mut()
            .expect("EffectNodeContext::gpu_encoder called without a GpuEncoder bound")
    }

    /// Mark this primitive as having accessed the GPU during the
    /// current `evaluate` call. Call once at the top of any code
    /// path that will dispatch compute or copy buffers / textures.
    ///
    /// The flag flips `ctx.gpu_accessed = true`. The executor reads
    /// it after `evaluate` returns to enforce the aliased-output
    /// contract — a primitive that declared `aliased_array_io` but
    /// never called this (or `gpu_encoder()`, which also flips the
    /// flag) clearly didn't dispatch the kernel that mutates the
    /// aliased buffer, so downstream consumers would read stale
    /// data. Debug builds panic loudly.
    ///
    /// Use this when you need the split-borrow `ctx.gpu.as_deref_mut()
    /// / ctx.state.as_deref_mut()` pattern (gpu + state + inputs
    /// referenced together) — the borrow checker can see each field
    /// is disjoint when you access them directly, but a single helper
    /// method that returned both refs would borrow the whole ctx and
    /// conflict with subsequent `ctx.inputs.*` reads.
    pub fn mark_gpu_accessed(&mut self) {
        self.gpu_accessed = true;
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
///     [`EffectNodeContext::state_store`] (feedback plus the scalar-envelope
///     family: compressor/sample-and-hold/envelope-decay/trigger-ease-to/
///     envelope-follower/inject-burst). Also the authoritative statefulness
///     signal `def_is_segment_stateless` consults (BUG-009).
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

    /// Post-frame capture phase for state-capture primitives. Called
    /// AFTER every node's `evaluate` has run for the frame — so by
    /// the time `late_capture` fires, the producer feeding any
    /// state-capture input port has already written THIS frame's
    /// output into the persistent back-edge slot. Reading that input
    /// here lets a stateful primitive snapshot the value it should
    /// emit on next frame's `evaluate`, giving a true 1-frame delay
    /// (matching ping-pong with end-of-frame swap).
    ///
    /// Only invoked when [`state_capture_input_ports`] returns a
    /// non-empty list — pure / stateless primitives have no reason
    /// to override this and pay no cost. The context exposes the
    /// same inputs and state store as `evaluate`; **outputs are not
    /// guaranteed to be acquired** (their slot may have been freed
    /// by the planner's `free_after` pass), so `late_capture`
    /// implementations should only read inputs and write to state.
    ///
    /// Default: no-op. Authors of new state-capture primitives MUST
    /// override this rather than encoding a capture inside `evaluate`
    /// — capture-before-producer in `evaluate` reads stale data
    /// (the 2-frame-delay class bug from the OilyFluid flicker
    /// incident).
    fn late_capture(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

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
    /// **Must match [`skip_passthrough_ports`]'s declaration** (or, for a
    /// variadic router, [`variadic_skip_passthrough_out`]'s) — the runtime
    /// uses this for the dynamic decision, the planner uses the static
    /// declaration for lifetime extension. Returning a port pair outside the
    /// declared set is a programmer error and may corrupt downstream reads
    /// in fan-out topologies.
    ///
    /// `wired_inputs` lists the input ports that have wires this build —
    /// a router whose selection depends on a wired control (mux's wired
    /// selector) uses it to decline the alias when the inline param isn't
    /// authoritative.
    ///
    /// [`variadic_skip_passthrough_out`]: Self::variadic_skip_passthrough_out
    fn skip_passthrough(
        &self,
        _params: &ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Variadic-router passthrough declaration: the OUTPUT port this node's
    /// [`skip_passthrough`](Self::skip_passthrough) may alias ANY of its wired
    /// texture inputs onto (mux's selected `in_N → out`). The planner extends
    /// every wired texture input's lifetime to the output's last reader, since
    /// it can't know statically which input the runtime will pick. Default
    /// `None` — fixed-pair nodes use [`skip_passthrough_ports`](Self::skip_passthrough_ports).
    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        None
    }

    /// Whether this node's outputs carry GPU resources (`Slot`s) *inside a
    /// CPU struct field* rather than on the wire's own port type — invisible
    /// to the planner's normal wire-based lifetime tracking, which only
    /// walks `Texture2D`/`Array` typed wires.
    ///
    /// SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D2: `node.scene_object` forwards
    /// mesh/map/instance `Slot`s inside the [`SceneObject`](crate::node_graph::scene_object::SceneObject)
    /// it emits. Without this declaration, the planner would see only the
    /// wire into `node.scene_object` and free those resource slots after
    /// `node.scene_object` runs — the same silent-corruption class
    /// `skip_passthrough_ports` exists to prevent for aliased textures, one
    /// level removed (through a struct field instead of a direct alias).
    ///
    /// When `true`, the planner (`execution_plan.rs`) extends the lifetime
    /// of every wired `Texture2D`/`Array` input to cover the last reader of
    /// this node's own output — the exact rule
    /// [`variadic_skip_passthrough_out`](Self::variadic_skip_passthrough_out)
    /// already implements for a mux's aliased texture inputs, applied here
    /// to a struct-carried resource instead of a direct alias.
    ///
    /// Default: `false` (most nodes' outputs are fully described by their
    /// wire's own port type).
    fn carries_resources(&self) -> bool {
        false
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
    fn aliased_array_io(&self) -> &[(&str, &str)] {
        &[]
    }

    /// Whether this node is a **liveness root** — must run every frame
    /// regardless of whether any per-frame wire consumes its output.
    ///
    /// The pruner (`Executor::compute_live_steps`) walks backwards from
    /// liveness roots through per-frame wires to decide which nodes
    /// dispatch each frame. A node that isn't reachable from any root
    /// is pruned to save GPU work.
    ///
    /// Most nodes are NOT roots — they're reachable from `system.final_output`
    /// via wires the pruner can see, and being live propagates naturally.
    /// A node IS a root when its effects reach next frame through a channel
    /// the wire walker can't see. Three current mechanisms qualify:
    ///
    /// 1. **`system.final_output`** writes to the host's display target,
    ///    which is the user-visible artefact. Always a root by definition.
    /// 2. **`aliased_array_io`** declares an input/output port pair that
    ///    resolve to one physical buffer at chain build. The dispatch
    ///    mutates that buffer in place; next frame's read picks up the
    ///    mutation through the persistent aliased slot. No per-frame wire
    ///    expresses the cross-frame consumer.
    /// 3. **`state_capture_input_ports`** declares input ports that capture
    ///    this-frame's producer output via `late_capture` into the
    ///    `StateStore` for next-frame `run` calls. The captured value
    ///    flows through persistent state, not through a per-frame wire.
    ///
    /// The default impl ORs the second and third together; nodes whose
    /// roots come from other channels (FinalOutput, future host-side
    /// effects like MIDI emit) override directly.
    ///
    /// New cross-frame mechanisms extend this default rather than adding
    /// a third inline criterion to the pruner. The pruner asks one
    /// question — "is this a liveness root?" — and every mechanism
    /// declares itself through this single method.
    fn is_liveness_root(&self) -> bool {
        !self.state_capture_input_ports().is_empty()
            || !self.aliased_array_io().is_empty()
    }

    /// Output Array port names whose buffer size must equal the
    /// canvas (`width × height` cells). The chain builder, on
    /// encountering one of these, allocates
    /// `canvas_w * canvas_h * item_size` bytes from the backend's
    /// canvas dims — `array_output_capacity` is bypassed for these
    /// ports. Used by scatter accumulators and any future primitive
    /// whose output must align pixel-for-pixel with the canvas.
    /// Default: empty.
    fn canvas_sized_array_outputs(&self) -> &[&str] {
        &[]
    }

    /// Whether the wires INTO this node should be treated as state
    /// captures rather than per-frame dependencies. When `true`:
    /// `topological_sort` ignores incoming edges (the node has zero
    /// in-degree regardless of how many wires terminate on it) and
    /// `Graph::connect`'s cycle check allows a wire to land on this
    /// node even if it'd otherwise close a cycle.
    ///
    /// The intended use is 1-frame-delay primitives — `node.feedback`,
    /// `node.array_feedback`, and any future reaction-diffusion / paint
    /// accumulator / smoke sim that closes its loop through `StateStore`
    /// rather than through wires. Their input is "what to remember for
    /// next frame," not "what to compute from this frame," so the
    /// dependency-graph view of a feedback chain like
    /// `source → mix → feedback → affine → mix` is a DAG once the
    /// `mix → feedback` edge is recognised as a state capture.
    ///
    /// Topologically, marked nodes run FIRST each frame: they emit
    /// last frame's captured value (their `out`) before any consumer
    /// runs, and they capture the wire-buffer value of their `in` —
    /// which still holds the previous frame's producer write — into
    /// their state. The effective delay is two frames between a
    /// downstream producer's write and the marked node's subsequent
    /// emit (one frame of buffer carry-over, one frame of state
    /// carry-over) — slightly longer than the old packaged
    /// `node.feedback` which evolved its blend internally each frame,
    /// but visually indistinguishable for typical feedback effects
    /// where evolution is governed by per-frame `amount`/`decay`.
    ///
    /// Default: `false`.
    fn breaks_dependency_cycle(&self) -> bool {
        !self.state_capture_input_ports().is_empty()
    }

    /// The subset of input ports that are state-capture wires (the
    /// per-port companion to [`breaks_dependency_cycle`]). The planner
    /// treats only these ports as cycle-break wires — their producer
    /// resources become persistent slots and they don't contribute to
    /// the topological in-degree of this node. Every OTHER input port
    /// is a regular per-frame dependency: it must be produced before
    /// this node runs, and the planner orders accordingly.
    ///
    /// Used to distinguish `node.feedback`'s `in` port (the closed
    /// loop) from sibling inputs like `seed` (a one-shot init source
    /// that has to run BEFORE feedback on the first frame). Without
    /// this distinction the planner couldn't schedule a seed producer
    /// upstream of the cycle-breaker: it'd see all incoming wires as
    /// state captures, pre-clear the seed slot to black, and feedback
    /// would init from black on the first frame even though the seed
    /// producer is wired and ready to compute.
    ///
    /// Default: empty (the node has no state-capture inputs). When a
    /// node overrides this to a non-empty list, `breaks_dependency_cycle`
    /// follows automatically via its default impl.
    fn state_capture_input_ports(&self) -> &[&str] {
        &[]
    }

    /// Output ports whose resources must be PERSISTENT — pre-acquired
    /// before the step loop, never pool-released, surviving across
    /// frames. The zero-copy feedback ping-pong rides on this: with
    /// both `node.feedback`'s `out` and its capture producer's output
    /// persistent, the two pinned slots ARE the ping-pong pair and a
    /// per-frame texture-handle swap replaces both full-canvas copies.
    /// Default: empty (outputs stay pooled transients).
    fn persistent_output_ports(&self) -> &[&str] {
        &[]
    }

    /// If `Some(port_name)`, this node is a branch-selector: only the
    /// upstream subgraph feeding the named input port needs to run
    /// this frame. The executor uses this to prune unselected branches
    /// — `node.switch_texture` returning `Some("in_2")` causes the
    /// in_0 / in_1 / in_3..7 producer chains to be skipped entirely
    /// for this frame's dispatch. Default: `None` (no pruning).
    ///
    /// `wired_inputs` lists every input port on this node that has a
    /// wire connected to it (the executor populates it from the plan's
    /// wire table before calling). Branch selectors whose selector
    /// input is itself wired — i.e. the selector value depends on a
    /// runtime-computed scalar that hasn't been resolved by the time
    /// the live-set is built — should return `None` and let every
    /// branch run, since we can't predict which one the wire will
    /// resolve to. Selectors driven by inline param values (the
    /// dominant live-perform case: outer-card slider → mux param)
    /// return `Some` to enable the optimisation.
    ///
    /// Semantic note: state-bearing nodes (e.g. `node.feedback`,
    /// accumulators) inside an unselected branch FREEZE — their
    /// producer doesn't run, so their persistent state isn't updated.
    /// When the branch becomes selected again, they pick up from the
    /// last value they wrote. This matches a switch-statement mental
    /// model where each `case` is its own independent sub-circuit.
    /// Authors who need state to advance regardless of selection
    /// should place the state-bearing node OUTSIDE the mux's input
    /// subgraphs.
    fn selected_input_branch(
        &self,
        _params: &ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<&'static str> {
        None
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

    /// Marks a node as holding trigger-EDGE latch state — a captured value
    /// or cycle index that only changes on a `trigger_count` edge and
    /// otherwise holds forever (`node.sample_and_hold`'s `held`,
    /// `node.clip_trigger_cycle` / `node.clip_trigger_index` /
    /// `node.frequency_ratio` / `node.cycle_table_row`'s cycle position,
    /// `node.trigger_gate`'s `output_count`, `node.trigger_ease_to`'s
    /// captured tween endpoints). Default `false`.
    ///
    /// BUG-104: unlike `clear_state` (fired on layer-idle / seek / project
    /// load for EFFECT chains), GENERATORS have no per-frame idle-reset
    /// pass — a generator instance is deliberately long-lived per layer so
    /// particle sims / feedback / accumulators survive clip changes
    /// (`docs/DECOMPOSING_GENERATORS.md` §9). That means a trigger latch
    /// inside a generator graph, once captured, silently outlives the
    /// "Trigger" card option being switched back off — the param that
    /// drove capture goes back to 0, but the captured value or cycle index
    /// never releases. `PresetRuntime::clear_trigger_state` uses this flag
    /// to release EXACTLY these nodes (both `clear_state`-owned instance
    /// fields AND their `StateStore` buckets) on transport stop / project
    /// load, without touching the broader persistent state (feedback,
    /// mip pyramids, particle buffers) a full `clear_state()` would wipe.
    /// Nodes that flag `true` should also override `clear_state()` if they
    /// hold the latch as an instance field (`extra_fields`); nodes whose
    /// latch lives only in the `StateStore` (e.g. `sample_and_hold`) don't
    /// need to — the store-level sweep covers them.
    fn is_trigger_latch(&self) -> bool {
        false
    }

    /// Rebuild any param-derived port lists after a parameter changes.
    ///
    /// Default no-op — almost every node has a fixed port shape declared
    /// at compile time and ignores this. **Variadic** nodes (a dynamic
    /// mux / pack / sum whose port COUNT is driven by a `num_inputs`-style
    /// param) override it to rebuild the `Vec<NodeInput>` / `Vec<NodeOutput>`
    /// that `inputs()` / `outputs()` return, so the new shape is visible to
    /// `compile()`, `validate()`, and the editor snapshot.
    ///
    /// Called by [`Graph::set_param`](crate::node_graph::Graph::set_param)
    /// right after a value is stored (covering both editor edits and JSON
    /// load, which applies params via `set_param`) and once by
    /// `NodeInstance::new` after defaults are installed. It is NOT called on
    /// the per-frame `set_param_unchecked` hot path — port counts are
    /// authoring-time state, never modulated per frame. The `inputs()`
    /// "same shape every call" contract still holds *within* a compile/run
    /// cycle: `reconfigure` only fires between edits, and a port-count
    /// change forces a recompile before the next run.
    fn reconfigure(&mut self, _params: &ParamValues) {}

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

    /// Whether the named `Texture2D` output wants a full mip chain on its
    /// backing texture. Same compile-time contract as
    /// [`output_format`](Self::output_format): the plan records it per
    /// resource, the backend allocates the slot with
    /// `floor(log2(max(w,h))) + 1` mip levels, and slot recycling keys on
    /// it so a mipped slot never aliases a flat one.
    ///
    /// The producer is responsible for filling levels 1.. (typically
    /// `generate_mipmaps` after writing level 0) — the executor does not
    /// regenerate them. Default `false`: mips cost ~33% extra memory and
    /// only material-map sources sampled under minification (IMPORT_FIDELITY
    /// F-P6: `node.gltf_texture_source`) need them.
    fn output_mipmapped(&self, _port: &str) -> bool {
        false
    }

    /// Background file IO still in flight. IoBridge file sources whose
    /// `evaluate()` spawns a background decode thread (`node.gltf_texture_
    /// source`, `node.hdri_source`, `node.image_folder`-family) return
    /// `true` from the frame the decode is triggered until the decoded
    /// result has been uploaded to its source texture — i.e. while the
    /// node's output does NOT yet reflect its `path` param. Consumed by
    /// `PresetRuntime::io_pending` → headless convergence loops
    /// (`render-import`, conformance tests): byte-stable frames don't count
    /// toward convergence while any decode is pending, because a source
    /// emitting stable black during a multi-second decode (a 74 MB 4k EXR)
    /// is indistinguishable from a settled frame by readback alone (G-P6
    /// gate-review fix, GLB_CONFORMANCE_DESIGN.md). Default `false` —
    /// pure-GPU nodes never have IO in flight.
    fn io_pending(&self) -> bool {
        false
    }

    /// The sampler ADDRESS MODE a fused region must bind so this atom's
    /// `Gather` inputs sample identically whether fused or standalone. The
    /// freeze compiler folds a gather atom into a `node.wgsl_compute` kernel
    /// that binds ONE shared sampler; that sampler's address mode has to match
    /// the one this atom creates in its own `run()`, or the fused look diverges
    /// at the texture edges (the fused kernel would sample clamp where the
    /// standalone atom wraps). Default `ClampToEdge` — the historical fused
    /// sampler, so every coincident / clamp-gather atom keeps it and all-clamp
    /// regions stay byte-identical. A gather atom whose sampling WRAPS (a
    /// toroidal fluid gradient, a seamless-tile warp) overrides this, reading
    /// the same param its `run()` reads (e.g. `wrap_mode`). The fused region
    /// only fuses gathers that agree on one mode (see the install pass), so
    /// returning a non-default mode here is honoured directly.
    fn fused_gather_sampler_mode(&self, _params: &ParamValues) -> manifold_gpu::GpuAddressMode {
        manifold_gpu::GpuAddressMode::ClampToEdge
    }

    /// STENCIL atoms only: whether, under these param values, every gather
    /// coordinate the body computes lands exactly on a texel center (integer
    /// tap offsets from the fragment's own texel). When true, a fused fetch is
    /// texel-exact — hardware sampling snaps to the texel, the manual bilinear
    /// degenerates to a corner — so the finder may absorb a producer chain even
    /// INSIDE a pure-texture feedback loop (with the chain tail q16'd, the
    /// loop stays bit-exact by induction, the tier-A argument). Fractional-tap
    /// shapes (Dynamic blur, variable-width) return the default `false` and
    /// keep in-loop absorption off (a ~1-ulp lerp gap would amplify).
    fn stencil_taps_texel_exact(&self, _params: &ParamValues) -> bool {
        false
    }

    /// If this node EMITS a [`Material`](crate::node_graph::material::Material)
    /// of a statically-known [`MaterialKind`], return it. The validator
    /// uses this to resolve a downstream renderer's
    /// [`conditional_requirements`](Self::conditional_requirements) at
    /// preset-load time — without a static answer here the runtime
    /// catches the missing-input case via [`EffectNodeContext::error`].
    ///
    /// Default: `None` (this node doesn't emit a Material, or its kind
    /// is dynamic — e.g. a future authored sub-graph material).
    fn emitted_material_kind(&self) -> Option<MaterialKind> {
        None
    }

    /// Conditional input-requirement rules. The validator checks each
    /// rule whose `on_material_kind` matches the wired material's kind
    /// (resolved via [`Self::emitted_material_kind`] on the source
    /// node), raising
    /// [`GraphError::ConditionalRequirementUnmet`](crate::node_graph::validation::GraphError::ConditionalRequirementUnmet)
    /// for each `required_inputs` entry that has no wire.
    ///
    /// Default: empty — most nodes have unconditional requirements
    /// (declared via [`NodePort::required`](crate::node_graph::ports::NodePort)).
    /// The bundled 3D mesh renderers override this to encode their
    /// per-MaterialKind input requirements (Phong/Pbr/Cel need a
    /// `light`; Pbr also needs an `envmap`; Unlit needs neither).
    fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
        &[]
    }

    /// Fusion classification for the freeze/fusion compiler (design doc §12).
    /// Defaults to [`FusionKind::Boundary`](crate::node_graph::freeze::classify::FusionKind::Boundary)
    /// — never fused — so the region-grower only folds nodes that explicitly
    /// opt in. Primitives set it via the `primitive!` macro's `fusion_kind:`
    /// field; the blanket impl forwards `P::FUSION_KIND`.
    fn fusion_kind(&self) -> crate::node_graph::freeze::classify::FusionKind {
        crate::node_graph::freeze::classify::FusionKind::Boundary
    }

    /// Why this node stays a fusion Boundary (design doc D4,
    /// `docs/GRAPH_TOOLING_DESIGN.md`). `None` by default. `primitive!`-
    /// authored primitives set it via the `boundary_reason:` field
    /// (forwarded from `P::BOUNDARY_REASON`); hand-`impl EffectNode`
    /// primitives override this method directly. Every registered
    /// primitive is expected to satisfy `is_fusable() XOR
    /// boundary_reason().is_some()` — enforced by
    /// `freeze::classify::tests::every_boundary_atom_declares_its_reason`.
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        None
    }

    /// How this node propagates the depth companion channel the "3D
    /// Shading" toggle synthesizes (design doc `docs/DEPTH_RELIGHT_DESIGN.md`
    /// D1) — `Inherit`, `Warp`, `CombineNearest`, `SourceHeight`, or
    /// `Terminal`; see [`DepthRule`](crate::node_graph::depth_rule::DepthRule)
    /// for the full semantics of each. **No default** — unlike
    /// [`fusion_kind`](Self::fusion_kind), every `EffectNode` impl must
    /// declare this explicitly. `primitive!`-authored primitives set it via
    /// the macro's REQUIRED `depth_rule:` field (forwarded from
    /// `P::DEPTH_RULE`); hand-`impl EffectNode` primitives (including the
    /// `system.source` / `system.final_output` / `system.generator_input`
    /// boundary nodes) override this method directly.
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule;

    /// The [`RangeContract`](manifold_core::effects::RangeContract) on
    /// this node's `param_name` param, if any — the real physical/
    /// mathematical boundary the value must not cross
    /// (`docs/PARAM_RANGE_CONTRACT_DESIGN.md`). `None` by default: the
    /// overwhelming majority of params have no contract, only a display
    /// hint (their declared `range` on [`ParamDef`]). `primitive!`-authored
    /// primitives set this via the optional `param_contracts:` macro field
    /// (forwarded from `P::PARAM_CONTRACTS`); hand-`impl EffectNode`
    /// primitives override this method directly. Enforced by the meta-test
    /// `every_range_contract_names_a_real_boundary`
    /// (`node_graph::freeze::classify`) — every declared contract must
    /// appear in that test's curated table with its reason.
    fn param_contract(&self, _param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        None
    }

    /// PURE node: output depends only on param values + wired inputs — no
    /// frame time, no `StateStore`, no randomness, no CPU/FFI side effects.
    /// The executor memoizes pure steps (constant-subgraph hoisting): when a
    /// pure step's params and input resources are unchanged since its last
    /// execute, the step is skipped and its held output slot serves
    /// consumers. Default `false` — opt in via the `primitive!` macro's
    /// `pure:` field only after verifying the contract against `run()`.
    fn is_pure(&self) -> bool {
        false
    }

    /// Data-driven skip, REPORTER side: did this frame's `evaluate` produce
    /// EMPTY output — zero detected blobs, zero spawned particles, zero live
    /// tracks? The executor queries this immediately after `evaluate` and
    /// marks the step's output resources empty for the frame; downstream
    /// steps that declare [`empty_skip_input_ports`](Self::empty_skip_input_ports)
    /// over those resources skip. Only meaningful right after `evaluate`
    /// (typically backed by a count the node already computed CPU-side).
    /// Default `false` — never reports empty.
    fn reports_empty_output(&self) -> bool {
        false
    }

    /// Data-driven skip, CONSUMER side: input ports whose content being EMPTY
    /// (per an upstream [`reports_empty_output`](Self::reports_empty_output)
    /// reporter, or transitively another skipped consumer) makes this node's
    /// whole evaluate a no-op. When every listed port is wired, was empty
    /// LAST frame, and is empty THIS frame, the executor skips the step
    /// entirely — no acquire, no evaluate — and marks its outputs empty too.
    /// The one-frame guard means the node always executes the FIRST empty
    /// frame (writing out its empty state — a zeroed array, a cleared
    /// overlay), so the held slots consumers read while it skips hold the
    /// empty content, never the last non-empty frame's (no ghosting).
    ///
    /// Contract for declaring: with the listed inputs empty, `run()` writes
    /// outputs that are themselves empty AND frame-invariant (no time-driven
    /// animation of the empty state), and the node carries no per-frame state
    /// that must keep evolving while inputs are empty (a track-ager or trail
    /// decay must NOT declare this). Default: empty — never skipped.
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &[]
    }

    /// REGISTER-HEAVY body: the atom's `wgsl_body` inlines enough code (a
    /// bespoke simplex, a large helper suite) that folding it into a
    /// multi-atom kernel pushes register pressure past the occupancy cliff
    /// and the fused region runs SLOWER than its standalone dispatches —
    /// measured on FluidSim2D, where euler+wrap+burst fused at 3.05 ms
    /// vs the same three atoms at 2.43 ms standalone (burst's inlined
    /// `arb_simplex_noise_2d`). `true` keeps the atom a fusion Boundary (its
    /// own dispatch, exactly its unfused behaviour) so register-light
    /// neighbours still fuse profitably around it. Default `false`; the perf
    /// gate stays the final never-worse arbiter either way.
    fn fusion_register_heavy(&self) -> bool {
        false
    }

    /// The scalar param that bounds this atom's LIVE element count (e.g.
    /// `active_count` on the particle integrators) — the value its `run()`
    /// dispatches by, leaving elements beyond it untouched. A fused buffer
    /// region whose members all agree on the wired source of this param caps
    /// its dispatch at that value instead of the buffer CAPACITY the generic
    /// `arrayLength` guard implies — FluidSim2D's euler+wrap fused
    /// kernel was iterating the full pool (2.69 ms) while the standalone
    /// dispatches covered only live particles (1.37 ms). Default `None` (no
    /// cap; capacity dispatch as before).
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        None
    }

    /// Optional fusable WGSL body fragment (a pure `fn body(...)`) the fusion
    /// codegen chains into one kernel and generates the standalone kernel from
    /// (single-source). `None` (default) = the primitive's own kernel is
    /// authoritative. Set via the macro's `wgsl_body:` field; forwards
    /// `P::WGSL_BODY`.
    fn wgsl_body(&self) -> Option<&'static str> {
        None
    }

    /// Per-texture-input read-semantics for the fusion codegen
    /// ([`InputAccess`](crate::node_graph::freeze::classify::InputAccess)),
    /// aligned to the TEXTURE inputs in declaration order. Default empty = every
    /// texture input is [`InputAccess::Coincident`] (resolution-robust sampler
    /// read). Set via the macro's `input_access:` field; forwards
    /// `P::INPUT_ACCESS`.
    fn input_access(&self) -> &'static [crate::node_graph::freeze::classify::InputAccess] {
        &[]
    }

    /// STENCIL-FETCH body ABI (stencil tier): the `wgsl_body` reads each
    /// `Gather` texture input via a free `fetch_<port>(uv) -> vec4<f32>`
    /// function the codegen defines (a real sample standalone / for a fused
    /// real external, or a recomputed upstream chain for a fused virtual
    /// source), instead of `(texture_2d, sampler)` body args. Default `false`;
    /// the macro forwards `P::STENCIL_FETCH`.
    fn stencil_fetch(&self) -> bool {
        false
    }

    /// Specialization tokens the `wgsl_body` references as free identifiers,
    /// each `(token, param_name)` resolved from a STATIC Enum/Int param. The
    /// freeze compiler substitutes the def's param value into the body text
    /// before parsing/fusing; the classifier keeps the atom a boundary when a
    /// listed param is binding-targeted or control-wired. Default empty; the
    /// macro forwards `P::WGSL_SPECIALIZATION`.
    fn wgsl_specialization(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Shared WGSL library snippets this primitive's `wgsl_body` depends on
    /// (e.g. `noise_common`'s simplex helpers). The standalone codegen prepends
    /// them; the BUFFER fusion codegen prepends the deduped union across a
    /// region's members so their helper calls resolve. Default empty; the macro
    /// forwards `P::WGSL_INCLUDES`.
    fn wgsl_includes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Frame-derived uniform field names (e.g. `dt_scaled`, `frame_count`) the
    /// atom's `run()` packs each frame from `EffectNodeContext` — NOT params,
    /// NOT graph wires. The buffer fusion finder keeps any atom that declares
    /// these a boundary (a fused `node.wgsl_compute` has no per-member `run()` to
    /// recompute them). Default empty; the macro forwards `P::DERIVED_UNIFORMS`.
    fn derived_uniforms(&self) -> &'static [&'static str] {
        &[]
    }

    /// Output port names written via `atomicAdd` (scatter accumulators) rather
    /// than a coincident `out[idx] = body(...)` element write. The buffer fusion
    /// finder keeps these atoms boundaries (the multi-atom codegen threads
    /// element registers, not atomics). Default empty; macro forwards
    /// `P::ATOMIC_OUTPUTS`.
    fn atomic_outputs(&self) -> &'static [&'static str] {
        &[]
    }

    /// Texture formats this primitive's input port can natively
    /// consume. Returns `None` (the default) to mean "any format" —
    /// the primitive runs against whatever the upstream producer
    /// emits, relying on Metal's sampler to handle the read.
    ///
    /// Override on primitives that genuinely require a specific
    /// precision class on input — e.g. a compute shader whose
    /// storage<read> binding declares `r32float` cannot read from
    /// an `rgba16float` texture without an explicit conversion pass.
    ///
    /// The format contract: when both the producer's
    /// [`output_format`](Self::output_format) AND the consumer's
    /// `accepted_input_formats` are declared, the validator at
    /// [`crate::node_graph::validation::validate`] requires the
    /// producer's format to appear in the consumer's accept list.
    /// Otherwise (one or both unconstrained) the wire is accepted —
    /// the unconstrained side promises to handle whatever shows up.
    ///
    /// This is the "producer declares, consumer accepts" contract —
    /// catches the silent format-mismatch class (fp32 producer wired
    /// into fp16 consumer that saturates) at compile time, with the
    /// same two-layer pattern as the array resource audit: cause-site
    /// error at `connect`, catch-all audit at `validate`.
    fn accepted_input_formats(
        &self,
        _port: &str,
    ) -> Option<&'static [manifold_gpu::GpuTextureFormat]> {
        None
    }

    /// Output texture dimensions for the named port. Returns `None` to
    /// let the compiler apply its default policy: "max of texture
    /// input dims, or canvas dims if there are no texture inputs."
    /// Override only on primitives whose output dims diverge from
    /// every input — `node.downsample` (output = input / factor) is
    /// the canonical case.
    ///
    /// `input_dims` lists the dims of every wired Texture2D / Texture3D
    /// input the compiler has already resolved (topo-ordered walk
    /// guarantees these are known by the time this node's outputs are
    /// processed). `canvas_dims` is the host's final-frame target
    /// dims — primitives that need a canvas-relative scale read it
    /// here rather than reaching for `Backend::canvas_dims` at
    /// dispatch time.
    ///
    /// Defaults: `None` for every primitive. Dim resolution then
    /// flows through `ExecutionPlan::resource_dim` to
    /// `Backend::acquire`, which keys its slot pool on
    /// `(PortType, GpuTextureFormat, dims)` so e.g. a quarter-res
    /// rgba16float slot won't alias with a full-res rgba16float slot.
    fn output_dims(
        &self,
        _port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &ParamValues,
    ) -> Option<(u32, u32)> {
        None
    }

    /// Canvas-relative output dim hint, used as a fallback when
    /// [`output_dims`] returns `None` (typically because the input is
    /// a state-capture back-edge whose dim isn't compile-time known).
    /// The runtime computes
    /// `(canvas_w * num / den, canvas_h * num / den)` and allocates
    /// the slot at that size — letting primitives like `node.downsample`
    /// land their output at `canvas / factor` even when fed from a
    /// feedback loop where the input dim isn't yet resolved at plan
    /// time.
    ///
    /// Default: `None`. Most primitives produce canvas-sized output
    /// and have no need to override. Multi-resolution primitives
    /// (`node.downsample`, future `node.upsample` / mip stages)
    /// override to express their compile-time-known scale relative
    /// to canvas.
    ///
    /// Read-priority order at slot acquire: concrete `output_dims`
    /// → `output_canvas_scale` → canvas-sized fallback. So when
    /// `output_dims` resolves from a known input chain, the canvas
    /// scale hint is ignored — chaining downsamples (`canvas →
    /// canvas/4 → canvas/16`) works correctly because the second
    /// downsample sees a concrete input dim from the first.
    fn output_canvas_scale(
        &self,
        _port: &str,
        _params: &ParamValues,
    ) -> Option<(u32, u32)> {
        None
    }

    /// Install a per-output-port canvas-relative scale override.
    /// Called by persistence to apply JSON-declared `outputCanvasScales`
    /// entries after a node is constructed. No-op on nodes whose scale
    /// is fixed at compile time (the default for nearly every
    /// primitive). Dynamic-shape primitives (`node.wgsl_compute`)
    /// override to store the per-port scale and surface it via the
    /// matching `output_canvas_scale` getter, recovering the legacy
    /// quarter-res render trick for expensive ray-trace kernels
    /// whose downstream sampler upscales.
    fn set_output_canvas_scale(&mut self, _port: &str, _scale: (u32, u32)) {}

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

    /// Dimensions for the named `Texture3D` output port, as
    /// `(width, height, depth)` in voxels. Mirror of
    /// [`array_output_capacity`] for the Texture3D port type — the JSON
    /// loader's chain-build code calls this once after the node's params
    /// are set, then allocates a `GpuTexture` of those dims (with format
    /// from [`output_format`]) and pre-binds it via
    /// `MetalBackend::pre_bind_texture_3d`.
    ///
    /// **Patterns mirror `array_output_capacity`**:
    /// - **Producer** — reads node-local `vol_res` / `vol_depth` params.
    ///   Default impl handles this pattern: returns
    ///   `(params["vol_res"], params["vol_res"], params["vol_depth"])`
    ///   when both are present. Most FluidSim3D-family primitives match
    ///   without an override.
    /// - **Transform (same-as-input)** — primitives that pass a Texture3D
    ///   through (blur, gradient) override and return the matching input
    ///   dim from `input_dims`.
    ///
    /// Returning `None` for a Texture3D output is a load-time error —
    /// pre-bound allocation is a hard contract. Same shape as
    /// `array_output_capacity`'s `None` semantics; the JSON loader
    /// emits a clean `UnsizedTexture3DOutput` error.
    fn texture_3d_output_dims(
        &self,
        port_name: &str,
        params: &ParamValues,
        _input_dims: &[(&str, (u32, u32, u32))],
    ) -> Option<(u32, u32, u32)> {
        let is_texture_3d_output = self
            .outputs()
            .iter()
            .any(|p| p.name == port_name && p.ty == crate::node_graph::ports::PortType::Texture3D);
        if !is_texture_3d_output {
            return None;
        }
        let vol_res = params.get("vol_res").and_then(|v| v.as_u32_clamped(1))?;
        let vol_depth = params
            .get("vol_depth")
            .and_then(|v| v.as_u32_clamped(1))
            .unwrap_or(vol_res);
        Some((vol_res, vol_res, vol_depth))
    }
}
