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
//! [`apply_param_bindings`]; topology changes (effect added /
//! removed / reordered / type-swapped, group enabled / disabled
//! toggle, group crossing the 1.0 wet/dry boundary, render-resolution
//! change) rebuild from scratch.
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
//! - N `apply_param_bindings` calls (host bindings + ctx-driven params)
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
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::effect::EffectContext;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, LastAppliedCache, MetalBackend,
    NodeInstanceId, ParamBinding, ParamValue, PortType, PrimitiveRegistry, ResourceId, Slot,
    Source, SpliceResult, StateStore, apply_binding_defaults, apply_param_bindings,
    chain_spec_by_id, compile,
    splice_def_into_chain,
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
/// [`ChainGraph::try_build`] returns `Some` whenever every enabled
/// effect has either a primitive mapping (via
/// [`primitive_id_for_effect`]) or registered legacy metadata (via
/// [`metadata_by_id`]) so it can be wrapped as a
/// [`LegacyPostProcessNode`]. Disabled groups skip their effects
/// (the effects are omitted from the chain graph entirely).
///
/// Effect groups with `wet_dry` ≠ 1.0 are handled via Mix
/// sub-graphs. Multi-segment groups (members in non-contiguous
/// chain positions) emit one Mix per segment.
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
    /// State store for stateful primitives that key per-owner state
    /// off `(node_id, owner_key)` rather than carrying it on the
    /// node instance directly. Today that's only `temporal::Feedback`,
    /// but any future primitive that uses the `StateStore` API will
    /// route through here. The store's lifetime is tied to this
    /// `ChainGraph` — when the graph rebuilds (topology change /
    /// resize), the store is dropped along with it, mirroring how
    /// instance-level state (e.g. Watercolor's `feedback` field) is
    /// also lost on rebuild. `clear_state()` calls `cleanup_all()`
    /// on the store so seek / project-load paths reset both styles
    /// of stateful primitive uniformly.
    state_store: StateStore,
}

struct EffectSlot {
    #[allow(dead_code)]
    effect_id: EffectId,
    effect_type: EffectTypeId,
    /// Index into the chain's `effects` slice at the time this slot
    /// was constructed. Topology rebuilds are triggered by any change
    /// to that slice's structural shape (effect added/removed/reordered,
    /// enabled-bit toggle, group enabled toggle, group crossing the
    /// 1.0 wet/dry boundary) — see [`compute_topology_hash`]. As long
    /// as the cached graph is reused, `effects[legacy_index]` is the
    /// same `EffectInstance` whose modulated `param_values` the
    /// renderer just updated.
    legacy_index: usize,
    /// Effect-local handles returned by `spec.splice` — names are
    /// scoped to this effect. `Cow<'static, str>` so canonical splices
    /// stay zero-allocation and user-edited divergent defs can hold
    /// owned strings off disk.
    handles: Vec<(std::borrow::Cow<'static, str>, NodeInstanceId)>,
    /// Resolved host bindings. Each entry has its `HandleNode` target
    /// already resolved to a runtime `Node` target via
    /// [`ParamBinding::resolve_handles`]. Paired with [`binding_cache`]
    /// so [`apply_param_bindings`] can skip writes when the outer
    /// value hasn't changed — that's the keystone of "inner edits
    /// survive a chain rebuild when the outer slot is at rest."
    resolved_bindings: Vec<ParamBinding>,
    binding_cache: LastAppliedCache,
    /// Where to apply ctx-driven params (time / beat). The first
    /// handle for now — composite effects with multiple ctx-driven
    /// workers will need a richer resolution rule when we get there.
    ctx_target_node: Option<NodeInstanceId>,
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
    /// dispatch path" if any active effect can't be constructed
    /// (no primitive mapping AND no legacy factory).
    ///
    /// Multi-segment wet/dry groups (groups whose enabled effects
    /// sit in non-contiguous chain positions) build successfully:
    /// the build loop's open/close-on-every-transition pattern
    /// emits one Mix sub-graph per segment, all registered under
    /// the same `EffectGroupId` so per-frame `wet_dry` refresh
    /// applies uniformly to every segment.
    pub fn try_build(
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        primitives: &PrimitiveRegistry,
        device: &GpuDevice,
        pool: Option<&TexturePool>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        // Indexed so we can capture each active effect's original
        // position in `effects` — used as a per-frame O(1) lookup key
        // (replaces the previous AHashMap<EffectId, &EffectInstance>
        // rebuild). Topology changes rebuild this graph, so the
        // captured indices stay valid for the cache's lifetime.
        let active_effects: Vec<(usize, &EffectInstance)> = effects
            .iter()
            .enumerate()
            .filter(|(_, fx)| {
                if !fx.enabled {
                    return false;
                }
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

        // Preflight: every active effect must declare a `ChainSpec`.
        // Effects without one can't be spliced — fall back to the per-
        // effect dispatch path so the chain doesn't render garbage.
        for (_, fx) in &active_effects {
            chain_spec_by_id(fx.effect_type())?;
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

        for (legacy_index, fx) in &active_effects {
            let fx_group_id = fx.group_id.as_deref();
            let fx_group: Option<&EffectGroup> =
                fx_group_id.and_then(|gid| groups.iter().find(|g| g.id.as_str() == gid));
            // Emit a Mix sub-graph for every enabled group with
            // effects, regardless of the current `wet_dry` value.
            // Critically, this avoids a topology rebuild when
            // `wet_dry` crosses 1.0 — modulation routines very
            // commonly drive it through the 1.0 boundary, and
            // rebuilding wipes primitive state (Bloom mip pyramids,
            // Watercolor feedback, etc.) every crossing.
            //
            // At `wet_dry == 1.0` the Mix shader's `lerp(dry, wet, 1.0)`
            // is the wet path verbatim — same output as a no-Mix
            // chain — at the cost of one extra single-pass shader
            // dispatch per group. Worth it: state preservation is a
            // hard correctness property, the per-frame compute cost
            // is bounded and small.
            let needs_mix = fx_group.map(|g| g.enabled).unwrap_or(false);

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

            // Look up the effect's `ChainSpec` and splice its workers
            // directly into the chain graph. Every shipping effect
            // declares a spec — see `crates/manifold-renderer/src/effects/`.
            let spec = chain_spec_by_id(fx.effect_type())?;
            if spec.is_skipped(fx) {
                // No workers added — previous output flows directly
                // to the next effect.
                continue;
            }
            // Divergent path: user-edited def replaces the canonical
            // splice. The user's wiring is materialized directly into
            // the chain graph the same way `spec.splice` would place
            // the canonical layout.
            let splice_result = if let Some(def) = &fx.graph {
                match splice_def_into_chain(
                    &mut graph,
                    (prev_node, prev_out_port),
                    def,
                    primitives,
                ) {
                    Some(r) => r,
                    None => {
                        eprintln!(
                            "[chain-graph] {} divergent graph failed to splice; \
                             falling back to canonical spec.",
                            fx.effect_type().as_str()
                        );
                        (spec.splice)(&mut graph, (prev_node, prev_out_port))
                    }
                }
            } else {
                (spec.splice)(&mut graph, (prev_node, prev_out_port))
            };
            let SpliceResult { output, handles } = splice_result;
            let mut resolved_bindings: Vec<ParamBinding> =
                Vec::with_capacity(spec.bindings.len());
            for b in spec.bindings {
                match b.resolve_handles(&handles) {
                    Some(rb) => resolved_bindings.push(rb),
                    None => eprintln!(
                        "[chain-graph] {}: ParamBinding `{}` references handle that splice \
                         did not register; this binding will not apply.",
                        spec.type_id.as_str(),
                        b.id,
                    ),
                }
            }
            let mut binding_cache = LastAppliedCache::new();
            binding_cache.seed_from_bindings(&resolved_bindings);
            // Make the cache's "Applied(default_value)" claim true:
            // plant each binding default into the inner node now, so
            // the per-frame `apply_param_bindings` skip-on-unchanged
            // check holds against an inner that already matches.
            // Without this, an effect whose binding default differs
            // from the inner primitive's `ParamDef::default` would
            // render at the primitive default until the outer slot
            // moves off its declared default (the "touch to update"
            // bug).
            apply_binding_defaults(&resolved_bindings, &mut graph, None);
            let ctx_target_node = handles.first().map(|(_, id)| *id);
            effect_nodes.push(EffectSlot {
                effect_id: fx.id.clone(),
                effect_type: fx.effect_type().clone(),
                legacy_index: *legacy_index,
                handles,
                resolved_bindings,
                binding_cache,
                ctx_target_node,
            });
            prev_node = output.0;
            prev_out_port = output.1;
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

        // Assign Texture2D resources to a small set of physical slots
        // via a lifetime-planner simulation. The source resource gets
        // its own dedicated slot (the input texture is `replace`d into
        // it each frame — sharing the slot with a write target would
        // corrupt the upstream caller's texture), every other resource
        // ping-pongs across `K` recycled slots driven by the plan's
        // `free_after` lists. K is typically 2 for a linear chain;
        // wet/dry groups bump it to 3 (the pre-group texture must stay
        // alive across the group's effects so the closing `Mix.a`
        // input can read it).
        let assignment = assign_texture2d_slots(&plan, source_resource);

        // Allocate exactly one RenderTarget per physical slot. Pool
        // the allocation when a pool is available — `MTLHeap`
        // sub-allocation recycles textures across topology rebuilds
        // (scene switches, effect adds/removes), avoiding fresh
        // `MTLDevice.newTexture` kernel calls per rebuild.
        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        if let Some(p) = pool {
            backend.set_texture_pool(p);
        }
        let mut slot_handles: Vec<Slot> = Vec::with_capacity(assignment.slot_count as usize);
        for slot_idx in 0..assignment.slot_count {
            let label = if slot_idx == assignment.source_slot.0 {
                "chain-graph-source"
            } else {
                "chain-graph-pingpong"
            };
            let rt = if let Some(p) = pool {
                RenderTarget::new_pooled(p, width, height, GRAPH_FORMAT, label)
            } else {
                RenderTarget::new(device, width, height, GRAPH_FORMAT, label)
            };
            slot_handles.push(backend.allocate_slot(rt));
        }
        // The simulator returned sim-slot indices in 0..K. allocate_slot
        // is called in order, so backend slot ids match sim ids 1:1.
        let resolve = |s: Slot| slot_handles[s.0 as usize];
        for (res_id, sim_slot) in &assignment.resource_to_slot {
            backend.bind_resource_to_slot(*res_id, resolve(*sim_slot));
        }
        let source_slot = resolve(assignment.source_slot);
        let output_slot = resolve(
            *assignment
                .resource_to_slot
                .get(&final_output_resource)
                .expect("plan output resource has an assigned slot"),
        );

        let topology_hash = compute_topology_hash(effects, groups, width, height);

        Some(Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            effect_nodes,
            group_mix_nodes,
            source_slot,
            output_slot,
            width,
            height,
            topology_hash,
            state_store: StateStore::new(),
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
        // `set_param_unchecked` skips the per-call linear scan over
        // the Mix node's `parameters()`: we built these nodes
        // ourselves at chain-graph construction, so `"amount"` is
        // guaranteed to resolve.
        for (group_id, mix_node) in &self.group_mix_nodes {
            if let Some(group) = groups.iter().find(|g| g.id == *group_id) {
                self.graph.set_param_unchecked(
                    *mix_node,
                    "amount",
                    ParamValue::Float(group.wet_dry),
                );
            }
        }

        // Refresh per-effect params via the binding apply path. The
        // skip-on-unchanged invariant in `LastAppliedCache` means
        // per-card edits to inner-node params survive when the outer
        // slot is at rest, and the outer reclaims control as soon as
        // it moves. Effects are looked up by their captured
        // `legacy_index` (stable across a topology-stable lifetime).
        for slot in &mut self.effect_nodes {
            let Some(fx) = effects.get(slot.legacy_index) else {
                // Index drifted (caller mutated `effects` without
                // letting the topology hash catch it). Tolerate
                // rather than panic on a live stage.
                continue;
            };
            apply_param_bindings(
                &slot.resolved_bindings,
                &[],
                &mut self.graph,
                None,
                &fx.param_values,
                &mut slot.binding_cache,
            );
            if let Some(node) = slot.ctx_target_node {
                apply_ctx_params_at(
                    &mut self.graph,
                    node,
                    &slot.effect_type,
                    ctx.time,
                    ctx.beat,
                );
            }
        }

        // Install the upstream input texture into the source slot —
        // no GPU copy. `GpuTexture::clone` is one atomic retain on the
        // underlying `MTLTexture`; the source slot's `RenderTarget`
        // adopts the cloned texture in place, dropping its previous
        // texture's retain. The Source node's evaluate is a no-op, so
        // the first downstream effect reads the upstream texture
        // directly via slot lookup. Eliminates the per-chain
        // `copy_texture_to_texture` (was ~600μs full-screen blit at 4K)
        // **and** keeps the active compute encoder alive across the
        // chain boundary (the blit would have ended it, forcing a
        // fresh compute encoder + cache loss on the first effect).
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("ChainGraph backend is MetalBackend");
        let ok = metal.replace_texture_2d(self.source_slot, input_texture.clone());
        debug_assert!(ok, "source slot pre-bound at build time");

        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
            // Forward host frame counter so legacy effects (DoF /
            // WireframeDepth / BlobTracking) dispatched via the
            // ChainGraph fast path can throttle correctly. Was
            // previously hardcoded to 0 in the adapter, breaking
            // throttle gates.
            frame_count: ctx.frame_count,
        };
        // Use the StateStore-aware execute path so stateful primitives
        // that key per-owner state off `(node_id, owner_key)` — today
        // only `temporal::Feedback`, but any future primitive using the
        // StateStore API — get the StateStore + owner_key they need.
        // The `with_gpu` variant passes `state: None, owner_key: 0`,
        // which makes those primitives panic.
        self.executor.execute_frame_with_state(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
            &mut self.state_store,
            ctx.owner_key,
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
    /// / project load so trails, feedback, and mip pyramids don't
    /// carry stale content across playback discontinuities. Also
    /// wipes the chain's `StateStore` so primitives that key state
    /// there (e.g. `temporal::Feedback`'s prev-frame buffer) reset
    /// alongside instance-local state.
    pub fn clear_state(&mut self) {
        // Collect node ids first so we can release the &self borrow
        // before calling get_node_mut on each.
        let mut nodes_to_clear: Vec<NodeInstanceId> = Vec::new();
        for slot in &self.effect_nodes {
            for (_, id) in &slot.handles {
                nodes_to_clear.push(*id);
            }
        }
        for node_id in nodes_to_clear {
            if let Some(inst) = self.graph.get_node_mut(node_id) {
                inst.node.clear_state();
            }
        }
        self.state_store.cleanup_all();
    }
}

/// Inject ctx-derived primitive-only params (`time` / `beat`) onto the
/// effect's `ctx_target_node`. A handful of primitives — Glitch,
/// Strobe, VoronoiPrism, Watercolor — expose these as regular params
/// that the splice runtime fills from the per-frame `EffectContext`
/// each frame so their shaders see the same clock the legacy path did.
fn apply_ctx_params_at(
    graph: &mut Graph,
    node_id: NodeInstanceId,
    effect_type: &EffectTypeId,
    time: f32,
    beat: f32,
) {
    let mut set = |name: &'static str, value: ParamValue| {
        let _ = graph.set_param(node_id, name, value);
    };
    match effect_type.as_str() {
        "Glitch" | "Watercolor" => set("time", ParamValue::Float(time)),
        "Strobe" | "VoronoiPrism" => set("beat", ParamValue::Float(beat)),
        _ => {}
    }
}

/// Topology hash — captures only the layout-affecting fields of
/// `effects` + `groups`. Per-frame param values, drivers,
/// envelopes, AND continuous wet/dry values are EXCLUDED so live
/// modulation / live wet-dry slider drags don't trigger rebuilds.
///
/// Every enabled group with effects always emits a Mix sub-graph
/// (see `try_build`), so `wet_dry`'s value — discrete OR
/// continuous — never affects topology. The previous design
/// hashed `(wet_dry < 1.0)`; rebuilds across that boundary wiped
/// primitive state (Bloom mip pyramids, Watercolor feedback) every
/// time modulation drove `wet_dry` through 1.0.
///
/// **Skip-on-zero state is layout-affecting.** `try_build` walks
/// active effects and drops any whose `ChainSpec::is_skipped(fx)`
/// returns `true`, so flipping that predicate (typically by dragging
/// `amount` off / onto 0) changes which effects appear in the graph.
/// We hash the predicate's current result per effect so the rebuild
/// fires when the user drags `amount` away from 0 — without it the
/// freshly-added effect would never enter the graph until the user
/// toggled `enabled` (which IS in the hash) to force a rebuild.
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
        // Phase 3: per-card graph divergence. Bumping `graph_version`
        // when an editing command mutates `fx.graph` flips this hash,
        // forcing a chain rebuild on the next frame so the renderer
        // re-runs `apply_graph_def` with the new def. Primitive state is
        // lost across the rebuild — acceptable because graph edits are
        // editing-time events, not performance-time.
        fx.graph_version.hash(&mut h);
        // Skip-on-zero predicate state — see the doc-comment above.
        // Effects without a `ChainSpec` registered are ignored here
        // (legacy fallback); `try_build` will short-circuit anyway.
        if let Some(spec) = chain_spec_by_id(fx.effect_type()) {
            spec.is_skipped(fx).hash(&mut h);
        }
    }
    for g in groups {
        g.id.as_str().hash(&mut h);
        g.enabled.hash(&mut h);
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
    graph.set_param(mix_id, "mode", ParamValue::Enum(0)).ok()?;
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
    graph.connect(last_effect, (mix_id, "b")).ok()?;
    Some((mix_id, "out"))
}

/// Result of `assign_texture2d_slots`: one physical slot per logical
/// resource (with sharing for non-overlapping lifetimes), plus the
/// dedicated source slot and the total slot count.
struct SlotAssignment {
    resource_to_slot: AHashMap<ResourceId, Slot>,
    /// Dedicated slot for the upstream input texture. Held across the
    /// frame (the chain `replace_texture_2d`s a clone of the input
    /// into this slot's `RenderTarget` each frame), never recycled
    /// for intermediate writes — sharing would corrupt the upstream
    /// caller's texture when a later effect writes its output.
    source_slot: Slot,
    /// Total physical slots needed = slots actually allocated.
    slot_count: u32,
}

/// Walk the plan in topological order, mirroring the executor's
/// acquire/release ordering, to compute the minimum set of physical
/// slots needed for every `Texture2D` resource. The `source_resource`
/// is bound to slot 0 up-front and never returned to the free pool
/// (so other resources can't write through it later).
///
/// The simulator's slot ids are dense `0..K`. The caller maps them to
/// real backend slots 1:1 via `allocate_slot`.
fn assign_texture2d_slots(plan: &ExecutionPlan, source_resource: ResourceId) -> SlotAssignment {
    let mut resource_to_slot: AHashMap<ResourceId, Slot> = AHashMap::default();
    let source_slot = Slot(0);
    resource_to_slot.insert(source_resource, source_slot);
    let mut next_slot: u32 = 1;
    let mut free_pool: Vec<Slot> = Vec::new();

    for step in plan.steps() {
        // Acquire output slots — pop from free pool or grow.
        for &(_, res_id) in &step.outputs {
            if res_id == source_resource {
                continue;
            }
            if !matches!(plan.resource_type(res_id), Some(PortType::Texture2D)) {
                continue;
            }
            let slot = free_pool.pop().unwrap_or_else(|| {
                let s = Slot(next_slot);
                next_slot += 1;
                s
            });
            resource_to_slot.insert(res_id, slot);
        }
        // Release dead resources — return slots to the free pool.
        for &res_id in &step.free_after {
            if res_id == source_resource {
                // Source slot is dedicated. Never recycled.
                continue;
            }
            if !matches!(plan.resource_type(res_id), Some(PortType::Texture2D)) {
                continue;
            }
            if let Some(&slot) = resource_to_slot.get(&res_id) {
                free_pool.push(slot);
            }
        }
    }

    SlotAssignment {
        resource_to_slot,
        source_slot,
        slot_count: next_slot,
    }
}

// Silence the dead-code warning for the `EffectMetadata` import —
// it's used through `metadata_by_id`'s return type but rustc treats
// the use as an alias.
#[allow(dead_code)]
type _EffectMetadataAlias = EffectMetadata;

#[cfg(test)]
mod multi_segment_tests {
    //! Regression tests for the multi-segment wet/dry group support in
    //! `ChainGraph::try_build`. A "multi-segment" group is one whose
    //! enabled effects sit in non-contiguous positions in the chain —
    //! e.g. group `g` contains effects at indices 0 and 2, with a
    //! non-group effect at index 1 between them.
    //!
    //! Pre-fix: `try_build` rejected this layout via the
    //! `enabled_groups_are_contiguous` preflight; the chain fell back
    //! to the legacy per-effect dispatcher.
    //!
    //! Post-fix: the build loop's open/close-on-every-transition
    //! pattern emits one Mix sub-graph per segment, each fed from the
    //! pre-segment output and feeding the post-segment input. All Mix
    //! nodes register under the same `EffectGroupId` in
    //! `group_mix_nodes`, so the per-frame `wet_dry` refresh sets the
    //! `amount` param on every segment uniformly.
    use super::*;
    use manifold_core::EffectTypeId;
    use manifold_core::effect_definition_registry;
    use manifold_core::effects::{EffectGroup, EffectInstance};
    use manifold_core::id::EffectGroupId;
    use std::sync::Arc;

    fn make_default(ty: EffectTypeId) -> EffectInstance {
        effect_definition_registry::create_default(&ty)
    }

    #[test]
    fn non_contiguous_group_builds_multi_segment_mix() {
        let device = Arc::new(GpuDevice::new());
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        // Chain: Invert(g1) → ChromaticAberration → Invert(g1)
        // Effects on either side belong to g1; the middle effect doesn't.
        let mut e1 = make_default(EffectTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(EffectTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(EffectTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            ChainGraph::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256);

        let cg = result.expect(
            "ChainGraph should build for a non-contiguous wet/dry group \
             (multi-segment Mix support)",
        );

        // Two segments → two Mix sub-graphs, both keyed to g1.
        assert_eq!(
            cg.group_mix_nodes.len(),
            2,
            "non-contiguous group with 2 segments must emit 2 Mix sub-graphs",
        );
        for (gid, _) in &cg.group_mix_nodes {
            assert_eq!(gid.as_str(), "g1");
        }
    }

    #[test]
    fn contiguous_group_still_builds_single_mix() {
        // Regression guard: the contiguous case still produces exactly
        // one Mix sub-graph.
        let device = Arc::new(GpuDevice::new());
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        let mut e1 = make_default(EffectTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let mut e2 = make_default(EffectTypeId::CHROMATIC_ABERRATION);
        e2.group_id = Some(g1_id.clone());
        let e3 = make_default(EffectTypeId::INVERT_COLORS);

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            ChainGraph::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256);

        let cg = result.expect("ChainGraph should build for contiguous group");
        assert_eq!(cg.group_mix_nodes.len(), 1);
    }

    #[test]
    fn three_segment_group_builds_three_mix_sub_graphs() {
        // Chain: Invert(g1) → Chroma → Invert(g1) → Chroma → Invert(g1)
        // Group g1 has three non-contiguous segments.
        let device = Arc::new(GpuDevice::new());
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");
        let mut e1 = make_default(EffectTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(EffectTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(EffectTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());
        let e4 = make_default(EffectTypeId::CHROMATIC_ABERRATION);
        let mut e5 = make_default(EffectTypeId::INVERT_COLORS);
        e5.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.3,
            parent_group_id: None,
        };

        let result = ChainGraph::try_build(
            &[e1, e2, e3, e4, e5],
            &[g1],
            &primitives,
            &device,
            None,
            256,
            256,
        );

        let cg = result.expect("ChainGraph should build for three-segment group");
        assert_eq!(cg.group_mix_nodes.len(), 3);
    }
}

#[cfg(test)]
mod binding_seed_tests {
    //! Regression: a freshly-built chain must plant each binding's
    //! declared `default_value` into its inner-node target. Otherwise
    //! the per-frame skip cache lies about what's been written and the
    //! card has to be "touched" to push the correct value through —
    //! see [`apply_binding_defaults`].
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::EffectTypeId;
    use manifold_core::effect_definition_registry;
    use manifold_core::effects::EffectInstance;
    use std::sync::Arc;

    fn make_default(ty: EffectTypeId) -> EffectInstance {
        effect_definition_registry::create_default(&ty)
    }

    /// SoftFocus is the canonical reproducer: its outer `radius`
    /// binding default is `6.0`, but the underlying `Blur` primitive's
    /// `ParamDef::default` is `4.0`. Without the seed pass, the inner
    /// node starts at `4.0` and the user has to touch the slider for
    /// the cache compare to diverge and the binding to actually write.
    #[test]
    fn soft_focus_inner_blur_starts_at_binding_default_not_primitive_default() {
        let device = Arc::new(GpuDevice::new());
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(EffectTypeId::SOFT_FOCUS_GRAPH);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256)
            .expect("SoftFocus chain should build");

        let slot = cg
            .effect_nodes
            .first()
            .expect("SoftFocus contributes one effect slot");
        let (_, blur_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "blur")
            .expect("SoftFocus splice registers a `blur` handle");
        let blur = cg
            .graph
            .get_node(*blur_id)
            .expect("blur node id resolves on the freshly-built graph");
        let radius = blur
            .params
            .get("radius")
            .copied()
            .expect("Blur primitive exposes `radius` param");

        assert_eq!(
            radius,
            ParamValue::Float(6.0),
            "Blur.radius must start at the SoftFocus binding default (6.0), \
             not the Blur primitive default (4.0). If it's 4.0 the binding-default \
             seed pass regressed and effect cards will need to be 'touched' \
             before they take their settings."
        );
    }
}

#[cfg(test)]
mod topology_hash_tests {
    //! Regression: the topology hash must include each effect's
    //! current `is_skipped` state. Without it, dragging an
    //! `amount` slider away from 0 doesn't trigger a chain rebuild,
    //! so a freshly-added effect (which starts at `amount = 0` for
    //! most types) never enters the graph until the user toggles
    //! `enabled` — visible as the "add effect → must toggle to
    //! work" symptom.
    use super::*;
    use manifold_core::EffectTypeId;
    use manifold_core::effects::EffectInstance;
    use manifold_core::effect_definition_registry;

    fn make_default(ty: EffectTypeId) -> EffectInstance {
        effect_definition_registry::create_default(&ty)
    }

    #[test]
    fn hash_changes_when_skip_predicate_flips() {
        // VoronoiPrism: outer `amount` default = 0.0 → `is_skipped`
        // returns true. Drag the slider to 0.5 and the hash MUST
        // change so the chain rebuilds and the effect enters the
        // graph.
        let mut fx = make_default(EffectTypeId::VORONOI_PRISM);
        assert_eq!(
            fx.param_values.first().map(|p| p.value),
            Some(0.0),
            "test fixture relies on VoronoiPrism.amount default = 0.0",
        );

        let hash_at_zero = compute_topology_hash(&[fx.clone()], &[], 256, 256);

        // Bump amount off 0 — same fingerprint as a user dragging the
        // slider on the effect card.
        fx.set_base_param(0, 0.5);
        let hash_at_half = compute_topology_hash(&[fx], &[], 256, 256);

        assert_ne!(
            hash_at_zero, hash_at_half,
            "topology hash must change when an effect's SkipMode::OnZero \
             predicate flips — otherwise the chain doesn't rebuild and \
             the user has to toggle enabled to bring the effect into the \
             graph. See the doc-comment on `compute_topology_hash`."
        );
    }

    #[test]
    fn stateful_effects_never_skip() {
        // Stateful effects must keep their workers alive across an
        // `amount → 0 → up` drag so their accumulated state (Feedback
        // prev-frame texture, Bloom mip pyramid, Watercolor ping-pong,
        // DNN worker spool, etc.) survives the bypass moment.
        // Tagging them `SkipMode::Never` is how we guarantee that.
        for ty in [
            EffectTypeId::STYLIZED_FEEDBACK,
            EffectTypeId::BLOOM,
            EffectTypeId::WATERCOLOR,
            EffectTypeId::DEPTH_OF_FIELD,
            EffectTypeId::WIREFRAME_DEPTH,
            EffectTypeId::BLOB_TRACKING,
            EffectTypeId::AUTO_GAIN,
        ] {
            let spec = chain_spec_by_id(&ty).unwrap_or_else(|| {
                panic!("{:?}: missing ChainSpec", ty);
            });
            assert!(
                matches!(spec.skip, crate::node_graph::SkipMode::Never),
                "{:?}: stateful effects must be SkipMode::Never so their \
                 per-instance state survives an amount → 0 → up slider drag",
                ty,
            );
        }
    }
}
