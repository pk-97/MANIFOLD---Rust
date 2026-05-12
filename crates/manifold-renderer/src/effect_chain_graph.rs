//! [`ChainGraph`] — one cached [`Graph`] per `EffectChain`.
//!
//! Each chain compiles its full effect sequence (every active
//! [`EffectInstance`], plus `Mix` sub-graphs for wet/dry groups)
//! into a single graph runtime instance: one [`Graph`], one
//! [`ExecutionPlan`], one [`MetalBackend`], one [`Executor`]. That's
//! one ping/pong recycle pool for the chain, one executor step loop
//! per frame, one input-texture pre-bind per frame — no per-effect
//! dispatch overhead.
//!
//! Primitive state (mip pyramids, feedback buffers, depth workers)
//! lives inside the boxed [`EffectNode`] owned by the cached
//! [`Graph`]. Per-frame param changes refresh in place via
//! [`refresh_effect_params_at`] / [`apply_ctx_params_at`]; topology
//! changes (effect added/removed/reordered/type-swapped, group
//! enabled/disabled toggle, group crossing the 1.0 wet/dry
//! boundary, render-resolution change) rebuild from scratch.
//!
//! ## Build-time wiring
//!
//! Linear sequence:
//!
//! ```text
//! Source ──▶ eff_1 ──▶ eff_2 ──▶ … ──▶ eff_n ──▶ FinalOutput
//! ```
//!
//! Wet/dry group with `wet_dry < 1.0` (spans effects `e_i..e_j`):
//!
//! ```text
//! pre_group ─┬─▶ e_i ──▶ … ──▶ e_j ──▶ Mix.b
//!            └────────────────────────▶ Mix.a
//! Mix.out (= lerp(dry, wet, wet_dry)) ─▶ next_node
//! ```
//!
//! ## Per-frame cost
//!
//! - 1 `copy_texture_to_texture` (upstream input → source slot)
//! - N `refresh_effect_params_at` / `apply_ctx_params_at` calls
//! - K `set_param` calls (one per Mix node, refreshing `amount`)
//! - 1 `execute_frame_with_gpu` covering N + K + 2 step iterations
//!   (Source + N effects + K Mix nodes + FinalOutput)
//! - 1 `texture_2d` lookup for the chain output
//!
//! The single `copy_texture_to_texture` is the only residual overhead
//! relative to the legacy chain's direct-from-input first-effect
//! dispatch: the backend's slot API takes owned `RenderTarget`s, not
//! borrowed `&GpuTexture`s, so the upstream input is materialised
//! into the source slot once per chain invocation.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

use crate::effect::EffectContext;
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    apply_ctx_params_at, compile, metadata_by_id, primitive_id_for_effect,
    refresh_effect_params_at, Backend, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph,
    LegacyPostProcessNode, MetalBackend, NodeInstanceId, ParamValue, PortType, PrimitiveRegistry,
    ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
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
    /// One slot per Mix node introduced for a wet/dry group. The
    /// Mix's `amount` param is set to the group's `wet_dry` value
    /// every frame (so dragging a wet/dry slider in the UI doesn't
    /// rebuild the graph). Keyed by `EffectGroupId` for the
    /// per-frame lookup.
    group_mix_nodes: Vec<(EffectGroupId, NodeInstanceId)>,
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
    /// Construct a chain graph from `effects` + `groups`. Groups
    /// with `wet_dry < 1.0` become `Mix` sub-graphs (the
    /// pre-group texture fans out into both the group's effects in
    /// series AND a `Mix.a` wire; the group's last effect feeds
    /// `Mix.b`; `Mix.amount = wet_dry`). Disabled groups skip
    /// their effects entirely.
    ///
    /// Returns `None` to signal "fall back to the per-effect
    /// dispatch path" if:
    /// - any active effect can't be constructed (no primitive
    ///   mapping AND no legacy factory),
    /// - a group with `wet_dry < 1.0` spans non-contiguous effect
    ///   positions (the legacy dispatch handles this via repeated
    ///   snapshot/lerp; this builder produces one Mix per
    ///   contiguous run, so multi-segment partial-wet-dry groups
    ///   would need multiple Mix sub-graphs and aren't supported
    ///   in this version).
    pub fn try_build(
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        primitives: &PrimitiveRegistry,
        device: &GpuDevice,
        width: u32,
        height: u32,
    ) -> Option<Self> {
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

        // Preflight: any group with wet_dry < 1.0 that spans
        // non-contiguous effect positions is currently unsupported.
        // Walk active_effects and check that each such group's
        // membership is a single contiguous run.
        if !groups_are_contiguous_for_partial_wet_dry(&active_effects, groups) {
            return None;
        }

        // Build the graph: Source → [eff_1 → eff_2 → … → eff_n,
        // with Mix sub-graphs straddling wet/dry groups] →
        // FinalOutput. State machine over `current_group_id` so
        // entering/exiting a partial-wet-dry group emits the right
        // fan-out / Mix wiring.
        let mut graph = Graph::new();
        let source_node = graph.add_node(Box::new(Source::new()));
        let mut effect_nodes: Vec<EffectSlot> = Vec::with_capacity(active_effects.len());
        let mut group_mix_nodes: Vec<(EffectGroupId, NodeInstanceId)> = Vec::new();

        let mut prev_node: NodeInstanceId = source_node;
        let mut prev_out_port: &'static str = "out";

        // Tracks the active partial-wet-dry group (if any). When set,
        // `pre_group` is the (node, port) feeding into the group's
        // first effect (the dry path's fan-out source).
        let mut open_group: Option<OpenGroup> = None;

        for fx in &active_effects {
            let fx_group_id = fx.group_id.as_deref();
            let fx_group: Option<&EffectGroup> = fx_group_id
                .and_then(|gid| groups.iter().find(|g| g.id.as_str() == gid));
            let needs_mix = fx_group
                .map(|g| g.enabled && g.wet_dry < 1.0)
                .unwrap_or(false);

            // Detect group transition.
            let same_open = open_group
                .as_ref()
                .map(|og| Some(og.group_id.as_str()) == fx_group_id);
            if same_open != Some(true) {
                // Close the previously-open partial-wet-dry group
                // (if any) by emitting its Mix sub-graph.
                if let Some(closing) = open_group.take() {
                    let (mix_id, mix_out) =
                        close_mix_group(&mut graph, &closing, (prev_node, prev_out_port))?;
                    group_mix_nodes.push((closing.group_id.clone(), mix_id));
                    prev_node = mix_id;
                    prev_out_port = mix_out;
                }
                // Open the new group if it needs a Mix.
                if needs_mix {
                    let group = fx_group.expect("needs_mix implies fx_group");
                    open_group = Some(OpenGroup {
                        group_id: group.id.clone(),
                        pre_node: prev_node,
                        pre_port: prev_out_port,
                        wet_dry: group.wet_dry,
                    });
                }
            }

            // Add the effect node and connect it.
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

        // Close any still-open partial-wet-dry group at chain end.
        if let Some(closing) = open_group.take() {
            let (mix_id, mix_out) =
                close_mix_group(&mut graph, &closing, (prev_node, prev_out_port))?;
            group_mix_nodes.push((closing.group_id.clone(), mix_id));
            prev_node = mix_id;
            prev_out_port = mix_out;
        }

        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect((prev_node, prev_out_port), (final_out, "in"))
            .ok()?;

        // Compile and find the resources we need to pin / read.
        let plan = compile(&graph).ok()?;
        let source_resource = output_resource(&plan, source_node, "out");
        let final_output_resource = output_resource(&plan, prev_node, prev_out_port);

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
            group_mix_nodes,
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
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> Option<&GpuTexture> {
        // Refresh Mix `amount` for every wet/dry group — picks up
        // live slider drags / modulation without rebuilding the graph.
        for (group_id, mix_node) in &self.group_mix_nodes {
            if let Some(group) = groups.iter().find(|g| g.id == *group_id) {
                let _ = self
                    .graph
                    .set_param(*mix_node, "amount", ParamValue::Float(group.wet_dry));
            }
        }

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
/// `effects` + `groups`. Per-frame param values, drivers,
/// envelopes, AND continuous wet/dry values are EXCLUDED so live
/// modulation / live wet-dry slider drags don't trigger rebuilds.
/// Only the boolean "is partial-wet-dry?" predicate enters the
/// hash, since that decides whether the group emits a Mix
/// sub-graph at all.
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
        // Only the "partial wet/dry" predicate enters the hash —
        // dragging wet_dry from 0.4 to 0.6 doesn't rebuild, since
        // we just refresh the Mix node's `amount` param. Crossing
        // the 1.0 boundary does rebuild (group transitions from
        // Mix-sub-graph mode to linear-series mode).
        (g.wet_dry < 1.0).hash(&mut h);
    }
    width.hash(&mut h);
    height.hash(&mut h);
    h.finish()
}

/// State tracked for an open partial-wet-dry group during
/// `try_build`'s walk over active effects. Captures the pre-group
/// node + port so the Mix's `a` (dry) input wires from the same
/// source as the group's first effect, and the group's `wet_dry`
/// value so the Mix's `amount` param can be set at build time.
struct OpenGroup {
    group_id: EffectGroupId,
    pre_node: NodeInstanceId,
    pre_port: &'static str,
    wet_dry: f32,
}

/// Emit the Mix sub-graph for a closing partial-wet-dry group:
/// `dry = pre_group_output`, `wet = last_effect_output`,
/// `out = lerp(dry, wet, wet_dry)`. Returns the Mix node id and
/// its output port (`"out"`).
fn close_mix_group(
    graph: &mut Graph,
    closing: &OpenGroup,
    last_effect: (NodeInstanceId, &'static str),
) -> Option<(NodeInstanceId, &'static str)> {
    let mix_id = graph.add_node(Box::new(Mix::new()));
    // Mode = Lerp (0) — matches legacy `WetDryLerpPipeline`'s
    // `lerp(dry, wet, wet_dry)`.
    graph
        .set_param(mix_id, "mode", ParamValue::Enum(0))
        .ok()?;
    graph
        .set_param(mix_id, "amount", ParamValue::Float(closing.wet_dry))
        .ok()?;
    // Mix.a = dry (pre-group input). Already wired into the
    // group's first effect via this same output port — output
    // ports can fan out to many input ports, so adding a second
    // consumer is legal.
    graph
        .connect((closing.pre_node, closing.pre_port), (mix_id, "a"))
        .ok()?;
    // Mix.b = wet (post-group result).
    graph
        .connect(last_effect, (mix_id, "b"))
        .ok()?;
    Some((mix_id, "out"))
}

/// Returns `false` if any partial-wet-dry group spans
/// non-contiguous positions in the active-effects sequence. The
/// build pipeline emits exactly one Mix sub-graph per contiguous
/// run; interleaved groups would need multiple Mix sub-graphs and
/// state-merging that's not implemented yet.
fn groups_are_contiguous_for_partial_wet_dry(
    active_effects: &[&EffectInstance],
    groups: &[EffectGroup],
) -> bool {
    use ahash::AHashSet;
    let partial: AHashSet<&str> = groups
        .iter()
        .filter(|g| g.enabled && g.wet_dry < 1.0)
        .map(|g| g.id.as_str())
        .collect();
    if partial.is_empty() {
        return true;
    }
    let mut seen_runs: AHashSet<&str> = AHashSet::default();
    let mut current_run: Option<&str> = None;
    for fx in active_effects {
        let gid = fx
            .group_id
            .as_deref()
            .filter(|gid| partial.contains(gid));
        if gid != current_run {
            if let Some(prev) = current_run.take()
                && !seen_runs.insert(prev)
            {
                // We already saw this group in an earlier run.
                return false;
            }
            current_run = gid;
        }
    }
    if let Some(prev) = current_run
        && !seen_runs.insert(prev)
    {
        return false;
    }
    true
}

// Silence the dead-code warning for the `EffectMetadata` import —
// it's used through `metadata_by_id`'s return type but rustc treats
// the use as an alias.
#[allow(dead_code)]
type _EffectMetadataAlias = EffectMetadata;
