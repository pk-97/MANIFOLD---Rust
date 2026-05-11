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
use manifold_core::effects::EffectInstance;
use manifold_core::id::EffectId;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::EffectContext;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::{
    apply_ctx_params, build_effect_graph, primitive_id_for_effect, refresh_effect_params, compile,
    ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, PortType, PrimitiveRegistry,
    ResourceId, Slot,
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
