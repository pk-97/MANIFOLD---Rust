//! Graph-runtime adapter for `EffectChain`'s per-effect dispatch.
//!
//! [`GraphEffectRunner`] wraps one legacy `EffectInstance` as a
//! long-lived graph-runtime executor. Each runner owns:
//!
//! - the canonical `Source → primitive → FinalOutput` [`Graph`] for
//!   its effect, built via the bridge in
//!   [`crate::node_graph::effect_graphs`];
//! - the compiled [`ExecutionPlan`];
//! - a [`MetalBackend`] with both `source` and `output` `Texture2D`
//!   resources pre-bound to internally-managed `RenderTarget`s;
//! - the [`Executor`] driving execution.
//!
//! The primitive's per-frame state (mip pyramids, feedback buffers,
//! depth workers) lives inside the boxed `EffectNode` owned by the
//! `Graph` — keeping the runner around frame-to-frame is what
//! preserves that state. The pattern mirrors `MirrorFX` /
//! `SoftFocusGraphFX` / `StylizedFeedbackFX`, which already use this
//! same shape internally for their own graphs.
//!
//! ## Per-effect cost
//!
//! [`GraphEffectRunner::apply`] does:
//!
//!  1. `swap_texture_2d` chain's current source `RenderTarget` into
//!     the backend's pre-bound source slot, retrieving the dummy;
//!  2. same swap for the target;
//!  3. param refresh on the live `Graph`;
//!  4. `Executor::execute_frame_with_gpu` (real GPU work — primitive
//!     samples directly from chain's source RT and writes directly
//!     into chain's target RT, no intermediate copies);
//!  5. swap both RTs back out of the backend, restoring the dummies.
//!
//! That makes the graph-runtime dispatch as close to native as
//! possible while keeping the cached graph + state-preserving
//! ownership model. The chain itself still pays one
//! `copy_texture_to_texture` at the very start of `apply_chain` (to
//! materialise the upstream `&GpuTexture` input into its first ping
//! RT), since the backend's slot API takes owned `RenderTarget`s, not
//! borrows. That single up-front copy is paid per chain invocation,
//! not per effect.
//!
//! ## Ownership
//!
//! [`EffectChain`] owns a [`GraphEffectCache`] map keyed by
//! [`EffectId`]. Adding an effect to a chain creates a new runner on
//! the next `apply_chain`; removing an effect leaves a stale entry
//! that gets cleaned up next time `prune` is called with the live
//! effect list.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::id::EffectId;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

use crate::effect::EffectContext;
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::{
    apply_ctx_params, apply_ctx_params_at, build_effect_graph, compile, metadata_by_id,
    primitive_id_for_effect, refresh_effect_params, refresh_effect_params_at, Backend,
    ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, LegacyPostProcessNode, MetalBackend,
    NodeInstanceId, PortType, PrimitiveRegistry, ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// One cached graph-runtime executor for a single effect instance
/// in an effect chain. See module docs.
pub struct GraphEffectRunner {
    /// Stable id of the underlying effect instance. Used for cache
    /// invalidation when a chain's effect list changes — same id
    /// across frames reuses the runner; a different id at the same
    /// position drops the old runner so its primitive state is reset.
    effect_id: EffectId,
    /// Effect type captured at construction. Used as a sanity check
    /// before each `apply` — a project file edit that swaps the
    /// effect type at a stable id would otherwise silently render
    /// through the wrong primitive.
    effect_type: EffectTypeId,
    graph: Graph,
    plan: ExecutionPlan,
    source_resource: ResourceId,
    output_resource: ResourceId,
    state: Option<RenderState>,
}

struct RenderState {
    executor: Executor,
    source_slot: Slot,
    output_slot: Slot,
    width: u32,
    height: u32,
}

/// Per-`EffectChain` cache of [`GraphEffectRunner`]s. Keyed by
/// `EffectId` so reorders preserve state and removals can be pruned.
#[derive(Default)]
pub struct GraphEffectCache {
    runners: AHashMap<EffectId, GraphEffectRunner>,
}

impl GraphEffectCache {
    pub fn new() -> Self {
        Self {
            runners: AHashMap::default(),
        }
    }

    /// Run one effect through the graph runtime, with the chain's
    /// current source/target `RenderTarget`s moved through the
    /// runner's pre-bound slots (no intermediate copies).
    ///
    /// Returns `(source, target, dispatched)`. The `RenderTarget`s
    /// are always returned to the caller — the chain reinstalls them
    /// into its ping/pong slots whether dispatch happened or not.
    /// `dispatched == false` means the effect has no primitive
    /// mapping yet and the caller should fall back to its legacy
    /// dispatch using the same RTs (their contents are still the
    /// original source / uninitialised target the caller handed in).
    ///
    /// Lazy-builds the runner on first call for an `EffectId`.
    /// Re-uses it on subsequent frames so primitive state persists.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        primitives: &PrimitiveRegistry,
        gpu: &mut GpuEncoder<'_>,
        source: RenderTarget,
        target: RenderTarget,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) -> (RenderTarget, RenderTarget, bool) {
        // Only attempt the graph path for effects with a primitive
        // mapping. Mirror / SoftFocusGraph / StylizedFeedback don't
        // have one (they're already graph-backed internally and run
        // via `PostProcessEffect::apply` directly).
        if primitive_id_for_effect(fx.effect_type()).is_none() {
            return (source, target, false);
        }

        // Cache key is the stable EffectId. The hash map's borrow
        // checker doesn't let us return an &mut while also handing
        // it the constructor's `?` short-circuit, so split into
        // two-phase lookup: ensure-then-mutate.
        let needs_insert = match self.runners.get(&fx.id) {
            Some(runner) => runner.effect_type != *fx.effect_type(),
            None => true,
        };
        if needs_insert {
            let Ok(runner) = GraphEffectRunner::new(fx, primitives) else {
                return (source, target, false);
            };
            self.runners.insert(fx.id.clone(), runner);
        }

        let runner = self
            .runners
            .get_mut(&fx.id)
            .expect("just inserted or already present");
        let (s, t) = runner.apply(gpu, source, target, fx, ctx);
        (s, t, true)
    }

    /// Drop runners whose effect ids are no longer in the chain.
    /// Bounded by the number of *removed* effects — usually zero
    /// per frame. Call once per `apply_chain` invocation.
    pub fn prune(&mut self, live_effects: &[EffectInstance]) {
        if self.runners.is_empty() {
            return;
        }
        let live: ahash::AHashSet<&EffectId> = live_effects.iter().map(|fx| &fx.id).collect();
        self.runners.retain(|id, _| live.contains(id));
    }

    /// Reset every runner's primitive state (called on seek so trails
    /// and feedback don't carry stale content across discontinuities).
    pub fn clear_state(&mut self) {
        for runner in self.runners.values_mut() {
            // Canonical 3-node layout: Source(0), primitive(1),
            // FinalOutput(2). Only the primitive owns transient
            // state worth clearing, but calling clear_state on
            // boundary nodes is harmless (default no-op).
            for raw in 0..3u32 {
                let id = crate::node_graph::NodeInstanceId(raw);
                if let Some(inst) = runner.graph.get_node_mut(id) {
                    inst.node.clear_state();
                }
            }
        }
    }

    /// Drop all runner state. Called on resolution change so the
    /// next `apply` rebuilds backends at the new size, and on
    /// shutdown to release pooled textures.
    pub fn drop_all(&mut self) {
        self.runners.clear();
    }
}

impl GraphEffectRunner {
    fn new(
        fx: &EffectInstance,
        primitives: &PrimitiveRegistry,
    ) -> Result<Self, crate::node_graph::EffectGraphError> {
        let graph = build_effect_graph(fx, primitives)?;
        let plan = compile(&graph).expect(
            "canonical 3-node graph from build_effect_graph must compile — \
             validation is implicit in the bridge's static topology",
        );

        // Resolve resource ids for Source.out and the primitive's
        // `out` port. In the canonical layout, runtime node ids are
        // 0 (Source), 1 (primitive), 2 (FinalOutput).
        let source_resource = output_resource(&plan, crate::node_graph::NodeInstanceId(0), "out");
        let output_resource = output_resource(&plan, crate::node_graph::NodeInstanceId(1), "out");

        Ok(Self {
            effect_id: fx.id.clone(),
            effect_type: fx.effect_type().clone(),
            graph,
            plan,
            source_resource,
            output_resource,
            state: None,
        })
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        source: RenderTarget,
        target: RenderTarget,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) -> (RenderTarget, RenderTarget) {
        let needs_build = match &self.state {
            None => true,
            Some(s) => s.width != ctx.width || s.height != ctx.height,
        };
        if needs_build {
            self.state = Some(RenderState::build(
                gpu.device,
                ctx.width,
                ctx.height,
                &self.plan,
                self.source_resource,
                self.output_resource,
            ));
        }
        let state = self.state.as_mut().expect("state initialized above");

        // Swap chain's source/target RTs INTO the backend's
        // pre-bound slots, taking the dummies out. The primitive
        // will sample directly from the chain's source RT and
        // write directly into the chain's target RT — zero
        // intermediate copies relative to the legacy dispatch.
        let prev_source = state
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("RenderState built with MetalBackend")
            .swap_texture_2d(state.source_slot, source)
            .expect("source slot was pre-bound at state-build time");
        let prev_target = state
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("RenderState built with MetalBackend")
            .swap_texture_2d(state.output_slot, target)
            .expect("output slot was pre-bound at state-build time");

        // Refresh primitive params from the (possibly modulated)
        // EffectInstance. Logged-and-ignored if metadata drifts;
        // primitive keeps its declared defaults in that case.
        if let Err(e) = refresh_effect_params(&mut self.graph, fx) {
            eprintln!(
                "[manifold-renderer] GraphEffectRunner: failed to refresh params for \
                 {} ({}): {e}",
                fx.effect_type().as_str(),
                self.effect_id,
            );
        }
        // Inject ctx-derived primitive-only params (Glitch.time,
        // Strobe.beat, VoronoiPrism.beat / source_width). The
        // legacy effect read these from EffectContext directly; the
        // primitive needs them as named param values.
        apply_ctx_params(
            &mut self.graph,
            &self.effect_type,
            ctx.time,
            ctx.beat,
            ctx.edge_stretch_width,
        );

        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        state
            .executor
            .execute_frame_with_gpu(&mut self.graph, &self.plan, frame_time, gpu);

        // Swap the chain's RTs BACK OUT of the backend, restoring
        // the dummies so the slots remain bound (the executor's
        // `acquire` is idempotent on existing bindings — these
        // dummies stay in place until the next swap-in).
        let source_back = state
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("RenderState built with MetalBackend")
            .swap_texture_2d(state.source_slot, prev_source)
            .expect("source slot was just populated by the inbound swap");
        let target_back = state
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("RenderState built with MetalBackend")
            .swap_texture_2d(state.output_slot, prev_target)
            .expect("output slot was just populated by the inbound swap");
        (source_back, target_back)
    }
}

impl RenderState {
    fn build(
        device: &GpuDevice,
        width: u32,
        height: u32,
        plan: &ExecutionPlan,
        source_resource: ResourceId,
        output_resource: ResourceId,
    ) -> Self {
        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        let mut source_slot: Option<Slot> = None;
        let mut output_slot: Option<Slot> = None;

        // Pre-bind every Texture2D resource — `without_device` mode
        // panics on lazy-alloc, and the canonical 3-node graph has
        // exactly two Texture2D resources (Source.out + primitive.out)
        // anyway. The loop is robust against future primitives that
        // introduce intermediates.
        for i in 0..plan.resource_count() {
            let id = ResourceId(i as u32);
            if !matches!(plan.resource_type(id), Some(PortType::Texture2D)) {
                continue;
            }
            let (label, is_source, is_output) = if id == source_resource {
                ("graph-effect-source", true, false)
            } else if id == output_resource {
                ("graph-effect-output", false, true)
            } else {
                ("graph-effect-intermediate", false, false)
            };
            let target = RenderTarget::new(device, width, height, GRAPH_FORMAT, label);
            let slot = backend.pre_bind_texture_2d(id, target);
            if is_source {
                source_slot = Some(slot);
            } else if is_output {
                output_slot = Some(slot);
            }
        }

        let executor = Executor::new(Box::new(backend));

        Self {
            executor,
            source_slot: source_slot.expect("source_resource is a Texture2D in the plan"),
            output_slot: output_slot.expect("output_resource is a Texture2D in the plan"),
            width,
            height,
        }
    }
}

/// Walk the plan to find the `ResourceId` produced by `node`'s named
/// output port. Mirrors the helper in `effects/mirror.rs` —
/// duplicated here because it's a 5-line plan utility and crossing
/// crate boundaries to share it would be premature.
fn output_resource(
    plan: &ExecutionPlan,
    node: crate::node_graph::NodeInstanceId,
    port: &str,
) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("plan: no output `{port}` on node {node:?}");
}

// ---------------------------------------------------------------------------
// ChainGraph — one Graph per EffectChain (linear, no wet/dry groups)
// ---------------------------------------------------------------------------

/// Whole-chain graph: one cached [`Graph`] containing every effect of
/// a chain wired in series, plus its compiled [`ExecutionPlan`],
/// [`Executor`], and slot bindings. Replaces N per-effect cached
/// graphs with one cached graph per chain.
///
/// ## Why one graph?
///
/// Per-effect cached graphs (the [`GraphEffectCache`] approach) pay
/// executor bookkeeping for 3 nodes per effect (`Source → primitive
/// → FinalOutput`) even though `Source` and `FinalOutput` are
/// no-op evaluates. For a 5-effect chain that's 15 step iterations
/// vs the 7 (1 + 5 + 1) we get here. The savings compound on long
/// chains.
///
/// The lifetime planner picks ping/pong placement automatically
/// across the whole chain; the chain no longer maintains its own
/// ping/pong RTs.
///
/// ## When this path is taken
///
/// [`ChainGraph::try_build_or_reuse`] returns `Some` only when:
/// - Every enabled effect has either a primitive mapping (via
///   [`primitive_id_for_effect`]) or registered legacy metadata
///   (via [`metadata_by_id`]) so it can be wrapped as a
///   [`LegacyPostProcessNode`].
/// - No effect group has `wet_dry < 1.0` (Mix sub-graphs come in
///   the next commit; chains with partial wet/dry still go through
///   the per-effect dispatch).
///
/// Disabled groups skip their effects (the effects are omitted from
/// the chain graph entirely).
///
/// ## State preservation
///
/// On topology change (effect added/removed/reordered, or effect
/// type swapped) the graph rebuilds from scratch. Primitive state
/// (mip pyramids, feedback buffers, depth workers) is lost across
/// the rebuild — acceptable because topology changes are
/// editing-time events, not performance-time. Per-frame param
/// changes (modulation, Ableton, user) refresh in place, no rebuild.
pub struct ChainGraph {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    /// One slot per effect node in the chain graph, in chain order.
    /// Same length as the active subset of effects at build time.
    /// Per-frame param refresh walks this in parallel with the live
    /// `effects` slice.
    effect_nodes: Vec<EffectSlot>,
    source_slot: Slot,
    /// `Slot` containing the chain's final output texture after
    /// `execute_frame_with_gpu`.
    output_slot: Slot,
    width: u32,
    height: u32,
    /// Hash of the topology this graph was built for. Compared
    /// each frame to decide whether to rebuild.
    topology_hash: u64,
}

struct EffectSlot {
    effect_id: EffectId,
    /// Effect type captured at build time. Combined with
    /// `effect_id` in the topology hash.
    effect_type: EffectTypeId,
    node_id: NodeInstanceId,
    /// `true` if this node is a primitive (via the bridge);
    /// `false` if it's a [`LegacyPostProcessNode`]. Drives the
    /// per-frame param refresh path (only primitive nodes need
    /// ctx-derived param injection; legacy adapters read ctx
    /// values from [`EffectContext`] directly).
    is_primitive: bool,
}

impl ChainGraph {
    /// Construct a chain graph from `effects` + `groups` if every
    /// active effect has a registered factory and no group is in
    /// partial-wet-dry mode. Returns `None` to signal "fall back to
    /// the per-effect dispatch path" otherwise.
    pub fn try_build(
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        primitives: &PrimitiveRegistry,
        device: &GpuDevice,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        // Preflight: any group with wet_dry < 1.0 means we need
        // Mix sub-graphs; punt back to the legacy chain.
        for g in groups {
            if g.enabled && g.wet_dry < 1.0 {
                return None;
            }
        }

        let active_effects: Vec<&EffectInstance> = effects
            .iter()
            .filter(|fx| {
                if !fx.enabled {
                    return false;
                }
                // Disabled-group effects are dropped.
                if let Some(gid) = fx.group_id.as_deref()
                    && let Some(group) = groups.iter().find(|g| g.id.as_str() == gid)
                    && !group.enabled
                {
                    return false;
                }
                true
            })
            .collect();

        if active_effects.is_empty() {
            return None;
        }

        // Preflight: every active effect must be constructable as
        // either a primitive or a legacy adapter. If anything is
        // unconstructable, fall back to the per-effect dispatch.
        for fx in &active_effects {
            let prim = primitive_id_for_effect(fx.effect_type())
                .map(|tid| primitives.contains(tid))
                .unwrap_or(false);
            let legacy = metadata_by_id(fx.effect_type()).is_some();
            if !prim && !legacy {
                return None;
            }
        }

        // Build the graph: Source → eff_1 → eff_2 → ... → eff_n → FinalOutput.
        let mut graph = Graph::new();
        let source_node = graph.add_node(Box::new(Source::new()));
        let mut effect_nodes: Vec<EffectSlot> = Vec::with_capacity(active_effects.len());

        // We chain via the previous node's "out" port → the next
        // node's input port. Primitive nodes name their input
        // `"in"` (per the §6.6 bridge convention). Legacy adapter
        // nodes name their input `"source"`. Track this so we can
        // pick the right wire.
        let mut prev_node: NodeInstanceId = source_node;
        let mut prev_out_port: &'static str = "out";

        for fx in &active_effects {
            let (node_id, is_primitive, input_port) =
                add_effect_node(&mut graph, fx, primitives, device)?;
            graph
                .connect((prev_node, prev_out_port), (node_id, input_port))
                .ok()?;
            effect_nodes.push(EffectSlot {
                effect_id: fx.id.clone(),
                effect_type: fx.effect_type().clone(),
                node_id,
                is_primitive,
            });
            prev_node = node_id;
            prev_out_port = "out";
        }

        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect((prev_node, prev_out_port), (final_out, "in"))
            .ok()?;

        // Compile and find the resources we need to pin / read.
        let plan = compile(&graph).ok()?;
        let source_resource = output_resource(&plan, source_node, "out");
        let final_output_resource = output_resource(&plan, prev_node, "out");

        // Pre-bind every Texture2D resource. `MetalBackend::without_device`
        // panics on lazy-alloc, so every Texture2D in the plan gets a
        // real `RenderTarget` here. The source slot retains its RT
        // across frames; the chain `install_texture_2d`s the input
        // into it each frame.
        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        let mut source_slot: Option<Slot> = None;
        let mut output_slot: Option<Slot> = None;
        for i in 0..plan.resource_count() {
            let id = ResourceId(i as u32);
            if !matches!(plan.resource_type(id), Some(PortType::Texture2D)) {
                continue;
            }
            let (label, is_source, is_output) = if id == source_resource {
                ("chain-graph-source", true, false)
            } else if id == final_output_resource {
                ("chain-graph-output", false, true)
            } else {
                ("chain-graph-intermediate", false, false)
            };
            let target = RenderTarget::new(device, width, height, GRAPH_FORMAT, label);
            let slot = backend.pre_bind_texture_2d(id, target);
            if is_source {
                source_slot = Some(slot);
            } else if is_output {
                output_slot = Some(slot);
            }
        }

        let topology_hash = compute_topology_hash(effects, groups, width, height);

        let _ = source_resource; // used during pre-bind only
        Some(Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            effect_nodes,
            source_slot: source_slot.expect("source resource was pre-bound"),
            output_slot: output_slot.expect("output resource was pre-bound"),
            width,
            height,
            topology_hash,
        })
    }

    /// Compare a cached graph's topology hash to the current chain's.
    /// Returns `true` if the same graph can be reused this frame.
    pub fn is_compatible(
        &self,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        width: u32,
        height: u32,
    ) -> bool {
        self.width == width
            && self.height == height
            && self.topology_hash == compute_topology_hash(effects, groups, width, height)
    }

    /// Run the cached chain graph against the upstream input texture.
    /// Returns a reference to the chain's output texture, or `None`
    /// if the executor couldn't be set up (should be unreachable in
    /// production).
    pub fn run(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        input_texture: &GpuTexture,
        effects: &[EffectInstance],
        ctx: &EffectContext,
    ) -> Option<&GpuTexture> {
        // Build a quick lookup from EffectId → &EffectInstance for
        // the param-refresh pass. The chain rebuilds on topology
        // change so the set of EffectIds in `effect_nodes` always
        // exists in `effects` (otherwise it'd be a stale graph and
        // the topology hash would have caught it).
        let by_id: AHashMap<&EffectId, &EffectInstance> =
            effects.iter().map(|fx| (&fx.id, fx)).collect();

        // Refresh per-effect params from the (possibly modulated)
        // EffectInstance.
        for slot in &self.effect_nodes {
            let Some(fx) = by_id.get(&slot.effect_id) else {
                // Shouldn't happen — topology hash invariant — but
                // tolerate by skipping rather than panicking on a
                // live stage.
                continue;
            };
            if let Err(e) = refresh_effect_params_at(&mut self.graph, slot.node_id, fx) {
                eprintln!(
                    "[manifold-renderer] ChainGraph: failed to refresh params for \
                     {} ({}): {e}",
                    fx.effect_type().as_str(),
                    slot.effect_id,
                );
            }
            if slot.is_primitive {
                apply_ctx_params_at(
                    &mut self.graph,
                    slot.node_id,
                    &slot.effect_type,
                    ctx.time,
                    ctx.beat,
                    ctx.edge_stretch_width,
                );
            }
        }

        // Copy the upstream input texture into the source slot. One
        // copy per chain invocation, regardless of effect count.
        // (See module docs for the borrow-vs-owned discussion.)
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("ChainGraph backend is MetalBackend");
        let source_tex = metal
            .texture_2d(self.source_slot)
            .expect("source slot pre-bound at build time");
        gpu.copy_texture_to_texture(input_texture, source_tex, ctx.width, ctx.height);

        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        self.executor.execute_frame_with_gpu(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
        );

        // The chain output is in the slot pre-bound to the last
        // effect's output resource.
        self.executor.backend().texture_2d(self.output_slot)
    }

    /// The chain's final output texture from the most recent
    /// [`Self::run`]. Returns `None` if the backend lookup fails
    /// (should be unreachable since `output_slot` was pre-bound at
    /// build time).
    pub fn output_texture(&self) -> Option<&GpuTexture> {
        self.executor.backend().texture_2d(self.output_slot)
    }

    /// Forwarded `clear_state` for each effect node — called on seek
    /// so trails / feedback don't carry stale content across
    /// playback discontinuities.
    pub fn clear_state(&mut self) {
        for slot in &self.effect_nodes {
            if let Some(inst) = self.graph.get_node_mut(slot.node_id) {
                inst.node.clear_state();
            }
        }
    }
}

/// Construct one effect node + connect it to the chain. Returns the
/// new node id, whether it's a primitive (vs legacy adapter), and
/// the name of its input port (`"in"` for primitives, `"source"`
/// for legacy adapter nodes).
fn add_effect_node(
    graph: &mut Graph,
    fx: &EffectInstance,
    primitives: &PrimitiveRegistry,
    device: &GpuDevice,
) -> Option<(NodeInstanceId, bool, &'static str)> {
    if let Some(prim_id) = primitive_id_for_effect(fx.effect_type()) {
        let node = primitives.construct(prim_id)?;
        let node_id = graph.add_node(node);
        return Some((node_id, true, "in"));
    }
    // Fall back to a `LegacyPostProcessNode` wrapping the legacy
    // factory. Constructs a fresh `PostProcessEffect` instance per
    // chain rebuild — state is lost on rebuild, same as primitives.
    let metadata = metadata_by_id(fx.effect_type())?;
    let factory = inventory::iter::<EffectFactory>
        .into_iter()
        .find(|f| f.id == *fx.effect_type())?;
    let inner = (factory.create)(device);
    let adapter = LegacyPostProcessNode::new(metadata, inner);
    let node_id = graph.add_node(Box::new(adapter));
    Some((node_id, false, "source"))
}

/// Topology hash — captures only the layout-affecting fields of
/// `effects` + `groups`. Per-frame param values, drivers, and
/// envelopes are EXCLUDED so live modulation doesn't trigger
/// rebuilds.
fn compute_topology_hash(
    effects: &[EffectInstance],
    groups: &[EffectGroup],
    width: u32,
    height: u32,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    for fx in effects {
        fx.id.as_str().hash(&mut h);
        fx.effect_type().as_str().hash(&mut h);
        fx.enabled.hash(&mut h);
        match fx.group_id.as_ref() {
            Some(g) => g.as_str().hash(&mut h),
            None => "".hash(&mut h),
        }
    }
    for g in groups {
        g.id.as_str().hash(&mut h);
        g.enabled.hash(&mut h);
        // wet_dry isn't a topology variable per se, but a change
        // from 1.0 → 0.9 (or vice versa) crosses the "needs Mix
        // sub-graph" threshold. Hash it so we rebuild and let
        // `try_build` re-check the precondition.
        g.wet_dry.to_bits().hash(&mut h);
    }
    width.hash(&mut h);
    height.hash(&mut h);
    h.finish()
}

// Silence the dead-code warning for the `EffectMetadata` import —
// it's used through `metadata_by_id`'s return type but rustc treats
// the use as an alias.
#[allow(dead_code)]
type _EffectMetadataAlias = EffectMetadata;
