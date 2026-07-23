//! Core [`PresetRuntime`] state, the live per-frame run/render path, and
//! the type it owns. The other preset_runtime submodules are facets of
//! this type. Extracted from preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

pub(super) const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

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
/// [`PresetRuntime::try_build`] returns `Some` whenever every enabled
/// effect has either a primitive mapping (via
/// [`primitive_id_for_effect`]) or registered legacy metadata (the
/// `EffectMetadata` catalog used to live in `node_graph::metadata`)
/// so it can be wrapped as a [`LegacyPostProcessNode`]. Disabled
/// groups skip their effects
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
pub struct PresetRuntime {
    pub graph: Graph,
    pub plan: ExecutionPlan,
    /// Last seen [`Graph::forced_outputs_epoch`]. When a live param write
    /// changes a node's forced-output set (BUG-317: `render_scene`'s
    /// `rt_enabled`/`temporal_upscale`), the compiled plan's
    /// `consumed_outputs` fold is stale — [`Self::refresh_plan_if_forced_outputs_changed`]
    /// recompiles before the frame executes. ResourceIds are assigned to
    /// EVERY output port in deterministic topo order regardless of
    /// consumption, so a recompile of the structurally-identical graph
    /// yields identical ids — pre-bound io slots and persistent-resource
    /// pins stay valid across the swap.
    pub(super) last_forced_outputs_epoch: u64,
    pub(super) executor: Executor,
    /// One slot per effect node in the chain graph, in chain order.
    /// Same length as the active subset of effects at build time.
    /// Per-frame param refresh walks this in parallel with the live
    /// `effects` slice.
    pub(super) effect_nodes: Vec<EffectSlot>,
    /// One slot per Mix node introduced for a wet/dry group. The
    /// Mix's `amount` param is set to the group's `wet_dry` value
    /// every frame (so dragging a wet/dry slider in the UI doesn't
    /// rebuild the graph). Keyed by `EffectGroupId` for the
    /// per-frame lookup.
    pub(super) group_mix_nodes: Vec<(EffectGroupId, NodeInstanceId)>,
    /// Input/output model. `Transform` (effect chain) installs an upstream
    /// input texture into a dedicated source slot each frame and owns its
    /// output slot (the host reads `output_texture()`). `Generate` (generator)
    /// has no input — it renders *into* a host-provided target texture
    /// installed at the `final_output` source slot each frame.
    pub(super) io: PresetIo,
    pub(super) width: u32,
    pub(super) height: u32,
    /// Hash of the topology this graph was built for. Compared
    /// each frame to decide whether to rebuild.
    pub(super) topology_hash: u64,
    /// Preset catalog generation this graph was built against (step 10
    /// hot-reload). The dispatcher compares it to the live
    /// [`crate::preset_loader::catalog_generation`] once per frame: when a
    /// preset `.json` is edited on disk the watcher bumps the generation,
    /// and any chain built against the old generation is rebuilt from the
    /// new defs. At rest the generation never moves, so the comparison is a
    /// single atomic load that always matches — no perform-path cost.
    pub(super) built_generation: u64,
    /// True when at least one segment of this chain was Pending (background
    /// compile in flight) at build time, so the chain spliced those cards
    /// per-card. The dispatcher rebuilds when the segment generation advances,
    /// picking up the fused winner (or the cached refusal) — the swap-in
    /// trigger. False once every segment resolved either way.
    pub(super) pending_segments: bool,
    /// [`crate::node_graph::freeze::install::segment_generation`] observed at
    /// build time. Compared by the dispatcher only while
    /// [`Self::pending_segments`] is set.
    pub(super) built_segment_generation: u64,
    /// State store for stateful primitives that key per-owner state
    /// off `(node_id, owner_key)` rather than carrying it on the
    /// node instance directly. Today that's only `temporal::Feedback`,
    /// but any future primitive that uses the `StateStore` API will
    /// route through here. The store's lifetime is tied to this
    /// `PresetRuntime` — when the graph rebuilds (topology change /
    /// resize), the store is dropped along with it, mirroring how
    /// instance-level state (e.g. Watercolor's `feedback` field) is
    /// also lost on rebuild. `clear_state()` calls `cleanup_all()`
    /// on the store so seek / project-load paths reset both styles
    /// of stateful primitive uniformly.
    pub(super) state_store: StateStore,
    /// Structured error log accumulated during `try_build` and the
    /// per-frame `run`. Each variant carries enough context
    /// (effect_id / effect_type / binding identity / node handle) for
    /// a future editor surface to attach the error to the affected
    /// effect card and inner node. Today this is consumed by tests
    /// and `errors()` for any host code that wants to surface them;
    /// the terminal log is the immediate user-visible benefit, written
    /// with the consistent `[chain-error]` prefix so logs grep
    /// cleanly.
    pub(super) errors: Vec<ChainError>,
    /// How the currently-previewed node's output should be rendered (flow wheel
    /// / lift / raw), derived from its descriptor + port name when
    /// [`Self::set_preview_target`] resolves a target. `Color` when nothing on
    /// this chain is previewed. Read by the host via [`Self::preview_encoding`].
    pub(super) preview_encoding: crate::node_graph::PreviewEncoding,

    // ---- Generator-only state (empty / None for effect-chain runtimes) ----
    /// Stable identity for the `GeneratorRegistry`. `Some` for generators
    /// (built via [`Self::from_def`] / [`Self::from_def_with_device`]); `None`
    /// for effect chains (which are addressed by `EffectId` per segment).
    pub(super) type_id: Option<PresetTypeId>,
    /// Texture format threaded through to placeholder allocation on a generator
    /// `resize`. `None` for effect chains and the mock-backend test path.
    pub(super) target_format: Option<GpuTextureFormat>,
    /// String-typed outer-card → inner-node bindings. Generators only — the
    /// shared float `apply` loop can't carry `String` params. Empty for chains.
    pub(super) string_bindings: Vec<StringBindingResolution>,
}

/// Input/output model for a [`PresetRuntime`]. The one genuine difference
/// between an effect chain and a generator: an effect transforms an upstream
/// input texture; a generator produces from nothing and writes into a
/// host-provided target.
pub(super) enum PresetIo {
    /// Effect chain. `source_slot` receives the upstream input texture each
    /// frame (via `replace_texture_2d`); `output_slot` holds the chain's final
    /// output texture, which the host reads via [`PresetRuntime::output_texture`].
    Transform { source_slot: Slot, output_slot: Slot },
    /// Generator. No input. The host installs its target texture into
    /// `final_output_slot` each frame; the graph renders into it. `Some(slot)`
    /// on the production path (real `MetalBackend`); `None` on the mock-backend
    /// test path (no GPU, slot never used).
    Generate {
        generator_input_id: NodeInstanceId,
        final_output_input_resource: ResourceId,
        final_output_slot: Option<Slot>,
    },
}

pub(super) struct EffectSlot {
    pub(super) effect_id: EffectId,
    pub(super) effect_type: PresetTypeId,
    /// Index into the chain's `effects` slice at the time this slot
    /// was constructed. Topology rebuilds are triggered by any change
    /// to that slice's structural shape (effect added/removed/reordered,
    /// enabled-bit toggle, group enabled toggle, group crossing the
    /// 1.0 wet/dry boundary) — see [`compute_topology_hash`]. As long
    /// as the cached graph is reused, `effects[legacy_index]` is the
    /// same `PresetInstance` whose modulated `param_values` the
    /// renderer just updated.
    pub(super) legacy_index: usize,
    /// Effect-local handles returned by `spec.splice` — names are
    /// scoped to this effect. `Cow<'static, str>` so canonical splices
    /// stay zero-allocation and user-edited divergent defs can hold
    /// owned strings off disk. Kept for state clearing (`clear_state`);
    /// binding resolution keys off [`Self::node_map`] instead.
    pub(super) handles: Vec<(std::borrow::Cow<'static, str>, NodeInstanceId)>,
    /// `(NodeId, NodeInstanceId)` for every spliced node — the binding
    /// resolution map. Built once at chain build by pairing each
    /// spliced runtime node with its stable [`NodeId`]. Static and user
    /// bindings resolve their target `NodeId` against this (see
    /// [`ResolvedBinding::from_static`] / [`ResolvedBinding::from_user`]),
    /// so a binding survives the node's handle changing under grouping.
    pub(super) node_map: Vec<(NodeId, NodeInstanceId)>,
    /// Group container `NodeId` → concrete inner producer `NodeId`, for the
    /// node-output preview. A group is a UI container that flattens away before
    /// the splice, so its own id is never in [`Self::node_map`]; selecting a
    /// collapsed group in the editor would otherwise preview nothing (black).
    /// Computed once at chain build from the pre-flatten preset def via
    /// [`manifold_core::flatten::group_output_producer_map`]; consulted by
    /// [`PresetRuntime::set_preview_target`] to substitute the group's primary
    /// texture-output producer. The third element is that output's interface
    /// port name (`forceField`), which drives the preview encoding. Empty for
    /// groupless effects.
    pub(super) group_preview_map: Vec<(NodeId, NodeId, String)>,
    /// Propagated preview data-kind per node `NodeId`, computed once at chain
    /// build from the flattened def via [`PreviewEncoding::propagate`]. Lets the
    /// node-output preview follow the *data*: a Gaussian Blur whose input was a
    /// force field resolves to `VectorField` here even though the blur's own
    /// descriptor says nothing. [`PresetRuntime::set_preview_target`] looks the
    /// previewed node up here before falling back to single-node `derive`.
    pub(super) preview_kinds: ahash::AHashMap<NodeId, crate::node_graph::PreviewEncoding>,
    /// Last `fx.graph_version` whose inner-node param overrides were pushed into
    /// the live graph. When the host's `graph_version` advances without a
    /// structure change (a value or position edit — no rebuild), `run` re-reads
    /// the def's node params and applies them in place via
    /// [`apply_inner_param_overrides`], so the edit lands without wiping
    /// primitive state. Seeded to `fx.graph_version` at build.
    pub(super) applied_graph_version: u32,
    /// The card-driven binding lifecycle — the resolved binding list + the
    /// skip-on-unchanged cache — shared with the generator runtime via
    /// [`BoundGraph`]. The static prefix (spec bindings via
    /// [`ResolvedBinding::from_static`]) comes first, the user tail (via
    /// [`ResolvedBinding::from_user`]) after; each binding reads its value from
    /// `PresetInstance.params` by `source_id`, so [`apply_bindings`] is
    /// order-independent (the static/user boundary is derived from
    /// [`BindingSource`] when the tail is rehydrated, not stored).
    ///
    /// The user tail is re-hydrated lazily when
    /// `effect.user_param_bindings_version` advances past
    /// [`Self::user_bindings_version`]; a reshape edit bumps `graph_version`,
    /// which forces a full chain rebuild (so the static prefix re-resolves
    /// from the preset spec); the cache clears (via
    /// [`BoundGraph::apply_inner_overrides`]) on a `graph_version` bump.
    pub(super) bound: BoundGraph,
    /// Last seen `PresetInstance.graph_version` for the user tail. User
    /// bindings live in the per-instance graph now, so a binding add /
    /// remove / reshape bumps the graph version. When the live effect's
    /// version differs, the per-frame apply path re-hydrates the user tail
    /// of [`Self::bindings`] from the synthesized binding list before
    /// applying.
    pub(super) user_bindings_version: u32,
    /// Content key of the card's EFFECTIVE def (edited graph or canonical) at
    /// build time — the state-harvest match key (docs/CHAIN_FUSION_DESIGN.md
    /// §5). A rebuild harvests a card's node state from the prior runtime
    /// only when `(effect_id, def_content_key)` match: same card, same inner
    /// content, so the old impls' baked configuration (ports, WGSL source,
    /// pipelines) is exactly what the new build constructed. Independent of
    /// fusion state — the same card fused and unfused shares one key, so
    /// closing the editor (unfused → fused rebuild) harvests too. `0` for
    /// cards inside a fused segment (stateless by eligibility, nothing to
    /// harvest).
    pub(super) def_content_key: u64,
    /// Effect-side `system.generator_input` node id, if the preset
    /// included one. Effects with a generator_input get per-frame
    /// scalars (time / beat / aspect / output dims) pushed to this
    /// node so the standard port-shadows-param wires propagate them
    /// to inner primitives — the same surface generators have.
    /// Lets effects react to project BPM, beat phase, output
    /// resolution, etc., without any per-effect Rust code.
    ///
    /// Every shipping effect that needs frame-context scalars uses this
    /// surface now.
    pub(super) generator_input_node: Option<NodeInstanceId>,
    /// `freeze::segment::card_prefix(seg_idx)` for a card that is a member of
    /// a fused multi-card SEGMENT, `""` otherwise. [`Self::node_map`] and
    /// `bound.fused_retarget` are keyed in the segment's `c{i}.`-prefixed
    /// namespace for a segment member (both were built from the concatenated
    /// segment def / its retarget map), while [`Self::run`]'s per-frame
    /// inner-override path reads each card's own (unprefixed) `fx.graph`.
    /// This prefix is threaded through
    /// [`BoundGraph::apply_inner_overrides_prefixed`] so that lookup lands in
    /// the right namespace for BOTH a surviving node (via `node_map`) and a
    /// fused-away one (via `fused_retarget`) — BUG-111.
    pub(super) card_prefix: String,
    /// Live D3 relight knob → runtime param/uniform-field writes, applied
    /// every frame when the card's toggle is on. Empty when relight is off.
    pub(super) relight_writes: Vec<RelightParamWrite>,
}

impl EffectSlot {
    /// Push the live relight knob values into the spliced graph. No-op if the
    /// card had relight off at build time (the template was never spliced).
    pub(super) fn apply_relight_params(&self, graph: &mut Graph, params: &RelightParams) {
        for w in &self.relight_writes {
            w.apply(graph, params);
        }
    }
}

/// The active (enabled, group-enabled) effects of a chain, with their original
/// indices into `effects`. Shared between the chain build and the project-load
/// segment prewarm so both walk the identical card list.
pub(super) fn chain_active_effects<'a>(
    effects: &'a [PresetInstance],
    groups: &[EffectGroup],
) -> Vec<(usize, &'a PresetInstance)> {
    effects
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
        .collect()
}

/// BUG-080 seam: a provisional manifest (built against an incomplete
/// registry, not yet reconciled) reaching chain build means a load/ingest
/// path skipped `reconcile_param_manifests()`. Loud in dev (panics), throttled
/// once per instance in release. Extracted so the seam behavior is directly
/// unit-testable without driving a full chain build — see
/// `docs/PARAM_MANIFEST_GATE_DESIGN.md` D2, INV-1.
pub(super) fn assert_manifest_gate(fx: &PresetInstance) {
    debug_assert!(
        !fx.manifest_provisional(),
        "BUG-080: provisional manifest reached PresetRuntime::try_build — a \
         load/ingest path skipped reconcile_param_manifests() (effect_id={:?})",
        fx.id,
    );
    if fx.manifest_provisional() {
        warn_provisional_manifest_once(&fx.id);
    }
}

/// BUG-080 D2: release-mode once-per-instance warn for a provisional
/// manifest reaching this seam. Shaped like the BUG-038 OSC-send throttle —
/// a plain instance is enough here since we only need "once ever per id",
/// not a reconnect transition. `debug_assert!` already screams in dev
/// builds; this is the release-mode signal that a load/ingest path skipped
/// `reconcile_param_manifests()`.
fn warn_provisional_manifest_once(id: &EffectId) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::HashSet<EffectId>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(id.clone()) {
        log::warn!(
            "BUG-080: provisional manifest reached PresetRuntime::try_build for \
             effect_id={id:?} — a load/ingest path skipped reconcile_param_manifests()"
        );
    }
}

/// The inputs [`PresetRuntime::try_build`] reads to construct a chain graph —
/// the effects/groups to build, the GPU device + pool to build against, and the
/// output dimensions (D9). Bundled so the build call reads two arguments (these
/// inputs + the optional `prior` runtime) instead of nine. Every field is a
/// `Copy` borrow, so `try_build` destructures it and its body stays unchanged.
pub struct ChainBuildInputs<'a> {
    /// The effects to build into the chain, in chain order.
    pub effects: &'a [PresetInstance],
    /// Effect groups (wet/dry `Mix` sub-graphs).
    pub groups: &'a [EffectGroup],
    /// Registry the atoms are constructed from.
    pub primitives: &'a PrimitiveRegistry,
    /// GPU device the pipelines compile against.
    pub device: &'a GpuDevice,
    /// Optional texture pool for intermediate targets.
    pub pool: Option<&'a TexturePool>,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// The effect the graph editor is watching, if any — built unfused even
    /// when the freeze toggle is on, so its intermediate node outputs exist
    /// for the authoring-time preview. `None` leaves the freeze gate untouched.
    pub preview_effect: Option<&'a EffectId>,
}

/// The per-frame timing/context [`PresetRuntime::set_frame_context`] pushes into
/// the `system.generator_input` node — seven values that are one fact (this
/// frame) (D9). `Copy`, so callers pass it by value.
#[derive(Clone, Copy)]
pub struct FrameContextInputs {
    /// Player time in seconds.
    pub time: f32,
    /// Transport position in beats.
    pub beat: f32,
    /// Output aspect ratio (width / height).
    pub aspect: f32,
    /// Number of triggers fired this frame.
    pub trigger_count: f32,
    /// Animation-clip progress in [0, 1].
    pub anim_progress: f32,
    /// Output width in pixels.
    pub output_width: f32,
    /// Output height in pixels.
    pub output_height: f32,
}

impl PresetRuntime {
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
    ///
    /// `prior` is the runtime this build replaces, when the dispatcher is
    /// rebuilding an existing chain. Cards whose content is unchanged harvest
    /// their node state (impl instances + StateStore buckets) from it, so a
    /// reorder / add / bypass / editor-close / fused-segment swap-in never
    /// resets a sim or a feedback trail — state identity is the card, not the
    /// chain position (docs/CHAIN_FUSION_DESIGN.md §5). `None` on first build;
    /// skipped automatically when dimensions changed.
    pub fn try_build(inputs: ChainBuildInputs<'_>, prior: Option<&mut Self>) -> Option<Self> {
        let ChainBuildInputs {
            effects,
            groups,
            primitives,
            device,
            pool,
            width,
            height,
            preview_effect,
        } = inputs;
        // Indexed so we can capture each active effect's original
        // position in `effects` — used as a per-frame O(1) lookup key
        // (replaces the previous AHashMap<EffectId, &PresetInstance>
        // rebuild). Topology changes rebuild this graph, so the
        // captured indices stay valid for the cache's lifetime.
        let active_effects: Vec<(usize, &PresetInstance)> = chain_active_effects(effects, groups);

        if active_effects.is_empty() {
            return None;
        }

        // Preflight: every active effect must have a `LoadedPresetView`
        // (JSON-loaded preset metadata). The chain build loop reads
        // bindings, skip mode, and the canonical splice all off the
        // view; an effect without one is unrunnable.
        for (_, fx) in &active_effects {
            assert_manifest_gate(fx);
            if loaded_preset_view_by_id(fx.effect_type()).is_none() {
                // BUG-079: this eprintln stays as a console diagnostic, but
                // it is no longer the only signal — the same root cause (a
                // preset def that never registered) is what leaves this
                // instance's `PresetInstance::template_unresolved()` true at
                // load time, which the loader folds into
                // `Project::load_report.unresolved_preset_templates` and
                // surfaces as an "opened with repairs" toast
                // (manifold-app/src/project_io.rs).
                eprintln!(
                    "[chain-build-fail] no LoadedPresetView for effect_type={:?} \
                     (effect_id={:?}) — chain build returns None, layer falls back \
                     to source passthrough",
                    fx.effect_type(),
                    fx.id,
                );
                return None;
            }
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
        // Structured error accumulator. Moves into `PresetRuntime::errors`
        // at the end of this function; mid-build failures push here so
        // the editor (and the consistent `[chain-error]` terminal log)
        // can show them tied to the affected effect.
        let mut errors: Vec<ChainError> = Vec::new();

        let mut prev_node: NodeInstanceId = source_node;
        let mut prev_out_port: &'static str = "out";

        // String outer-card bindings accumulated across every effect in the
        // chain (parity with the generator Generate path). Effects can expose
        // String params (font family, text mode) exactly as generators do;
        // resolved per-effect against its splice `node_map` below.
        let mut chain_string_bindings: Vec<StringBindingResolution> = Vec::new();

        // Tracks the active partial-wet-dry group (if any). When set,
        // `pre_group` is the (node, port) feeding into the group's
        // first effect (the dry path's fan-out source).
        let mut open_group: Option<OpenGroup> = None;

        // ── Chain fusion (docs/CHAIN_FUSION_DESIGN.md): plan splice units. ──
        // A maximal run of ≥2 adjacent eligible cards becomes a SEGMENT when a
        // compiled + gate-approved fused view exists in the content-keyed
        // cache. A miss enqueues a background compile and the run splices
        // per-card this build — byte-identical to the no-fusion path — then a
        // later rebuild (the dispatcher's segment-generation check) swaps the
        // fused segment in. Eligibility (v1 boundaries): watched card, any
        // grouped card (wet/dry Mix seams), skip-capable cards, stateful cards
        // (positional namespacing must never key state by chain position),
        // string-bound cards.
        use crate::node_graph::freeze::install as freeze_install;
        enum SpliceUnit {
            Card(usize),
            Segment {
                /// Indices into `active_effects` of the fused member cards, in
                /// chain order (currently-skipped cards excluded — they splice
                /// nothing on the per-card path either).
                cards: Vec<usize>,
                view: std::sync::Arc<freeze_install::SegmentView>,
            },
        }
        // Skip note: `OnZero` skip in this runtime is STATIC per build — the
        // predicate is in `compute_topology_hash`, a flip rebuilds the chain,
        // and a skipped card isn't spliced at all. So a fused segment never
        // contains skip logic: a currently-skipped card is TRANSPARENT
        // (excluded from the segment without breaking the run — exactly the
        // adjacency the per-card path produces), a currently-active OnZero
        // card is an ordinary member, and each skip state is its own segment
        // content key. Skip semantics ride the existing rebuild mechanism,
        // identically to the per-card path.
        //
        // Eligibility + run-scan live in `classify_segment_member` /
        // `segment_run` (module scope) — shared with the project-load
        // prewarm so the two can never disagree about what forms a segment.
        let mut pending_segments = false;
        let mut units: Vec<SpliceUnit> = Vec::with_capacity(active_effects.len());
        if freeze_install::chain_fusion_enabled() {
            let members: Vec<SegmentMember> = active_effects
                .iter()
                .map(|(_, fx)| classify_segment_member(fx, preview_effect, primitives))
                .collect();
            let mut i = 0;
            while i < active_effects.len() {
                if members[i] != SegmentMember::Fuse {
                    units.push(SpliceUnit::Card(i));
                    i += 1;
                    continue;
                }
                let (j, fuse_idxs) = segment_run(&members, i);
                if fuse_idxs.len() < 2 {
                    units.extend((i..j).map(SpliceUnit::Card));
                    i = j;
                    continue;
                }
                let cards = build_segment_cards(
                    &fuse_idxs, &active_effects, primitives,
                );
                match freeze_install::fused_segment_view_for(&cards) {
                    freeze_install::SegmentLookup::Ready(view) => {
                        // Skipped (transparent) cards inside the run splice
                        // nothing — emit them as plain cards so any future
                        // skip-state bookkeeping sees them, then the segment.
                        units.extend(
                            (i..j)
                                .filter(|&k| members[k] == SegmentMember::Transparent)
                                .map(SpliceUnit::Card),
                        );
                        units.push(SpliceUnit::Segment { cards: fuse_idxs, view });
                    }
                    freeze_install::SegmentLookup::Pending => {
                        pending_segments = true;
                        units.extend((i..j).map(SpliceUnit::Card));
                    }
                    freeze_install::SegmentLookup::Refused => {
                        units.extend((i..j).map(SpliceUnit::Card));
                    }
                }
                i = j;
            }
        } else {
            units.extend((0..active_effects.len()).map(SpliceUnit::Card));
        }

        for unit in &units {
            let (legacy_index, fx) = match unit {
                SpliceUnit::Segment { cards: seg_cards, view } => {
                    // Segment cards are ungrouped — close any open wet/dry
                    // group exactly as an ungrouped card would.
                    if let Some(closing) = open_group.take() {
                        let Some((mix_id, mix_out)) =
                            close_mix_group(&mut graph, &closing, (prev_node, prev_out_port))
                        else {
                            eprintln!(
                                "[chain-build-fail] close_mix_group failed before segment \
                                 for group_id={:?}",
                                closing.group_id,
                            );
                            return None;
                        };
                        group_mix_nodes.push((closing.group_id.clone(), mix_id));
                        prev_node = mix_id;
                        prev_out_port = mix_out;
                    }
                    eprintln!(
                        "[freeze] chain segment → FUSED ({} cards, one splice)",
                        seg_cards.len()
                    );
                    let Some(SpliceResult { output, handles, generator_input_id: _ }) =
                        splice_def_into_chain(
                            &mut graph,
                            (prev_node, prev_out_port),
                            &view.def,
                            primitives,
                            // No per-member toggle to honor here — a
                            // relight-on card is excluded from fusion
                            // eligibility (`classify_segment_member`), so
                            // every member folded into `view.def` has
                            // `relight == false`.
                            None,
                        )
                    else {
                        // Near-unreachable: compile_segment_view verified the def
                        // builds. Follow the canonical-splice-failure precedent
                        // (None → passthrough this frame; the next rebuild
                        // renders per-card).
                        eprintln!("[chain-build-fail] fused segment splice failed");
                        return None;
                    };
                    let node_map: Vec<(NodeId, NodeInstanceId)> = handles
                        .iter()
                        .filter_map(|(_, id)| {
                            graph.get_node(*id).map(|inst| (inst.node_id.clone(), *id))
                        })
                        .collect();
                    for (seg_idx, k) in seg_cards.iter().enumerate() {
                        let (legacy_index, fx) = &active_effects[*k];
                        let base_view = loaded_preset_view_by_id(fx.effect_type())
                            .expect("preflight guarantees view");
                        let prefix = crate::node_graph::freeze::segment::card_prefix(seg_idx);
                        let card_static = &view.card_bindings[seg_idx];
                        let user_bindings = fx.user_param_bindings();
                        let mut bindings: Vec<ResolvedBinding> =
                            Vec::with_capacity(card_static.len() + user_bindings.len());
                        for b in card_static.iter() {
                            match ResolvedBinding::from_static(b, &node_map) {
                                Some(rb) => bindings.push(rb),
                                None => record_chain_error(
                                    &mut errors,
                                    ChainError::StaticBindingHandleMissing {
                                        effect_type: base_view.type_id.clone(),
                                        binding_id: b.id.to_string(),
                                    },
                                ),
                            }
                        }
                        for core in user_bindings.iter() {
                            // Repoint into the segment namespace: a fused-away
                            // target goes through the segment retarget map; a
                            // surviving target resolves by prefixing its stable
                            // node id.
                            let prefixed_key = (
                                format!("{prefix}{}", core.node_id.as_str()),
                                core.inner_param.clone(),
                            );
                            let mut c = core.clone();
                            if let Some((fused_id, field)) = view.retarget.get(&prefixed_key) {
                                c.node_id = fused_id.clone();
                                c.inner_param = field.clone();
                                c.convert = freeze_install::convert_for_fused_field(c.convert);
                            } else {
                                c.node_id = NodeId::new(prefixed_key.0.clone());
                            }
                            match ResolvedBinding::from_user(&c, &graph, &node_map) {
                                Some(rb) => bindings.push(rb),
                                None => record_chain_error(
                                    &mut errors,
                                    ChainError::UserBindingResolveFailed {
                                        effect_id: fx.id.clone(),
                                        effect_type: fx.effect_type().clone(),
                                        binding_id: core.id.clone(),
                                        node_id: core.node_id.to_string(),
                                        inner_param: core.inner_param.clone(),
                                        rehydrate: false,
                                    },
                                ),
                            }
                        }
                        let mut bound = BoundGraph::new(bindings, &mut graph);
                        // Carry the segment view's retarget (already prefixed
                        // in this card's `c{i}.` namespace) so an in-place
                        // inner-param edit on a fused-away node reaches the
                        // live kernel — the segment analog of the BUG-006
                        // fix above (BUG-111). Empty when nothing in this
                        // segment fused away.
                        bound.fused_retarget = view.retarget.clone();
                        let generator_input_node = node_map.iter().find_map(|(nid, inst)| {
                            (nid.as_str().starts_with(prefix.as_str())
                                && graph.get_node(*inst).is_some_and(|n| {
                                    n.node.type_id().as_str()
                                        == GENERATOR_INPUT_TYPE_ID
                                }))
                            .then_some(*inst)
                        });
                        let card_handles: Vec<(std::borrow::Cow<'static, str>, NodeInstanceId)> =
                            handles
                                .iter()
                                .filter(|(name, _)| name.starts_with(prefix.as_str()))
                                .cloned()
                                .collect();
                        let relight_writes = build_relight_writes(
                            fx.relight_active(),
                            &card_handles,
                            &node_map,
                            &bound.fused_retarget,
                            &prefix,
                        );
                        effect_nodes.push(EffectSlot {
                            effect_id: fx.id.clone(),
                            effect_type: fx.effect_type().clone(),
                            legacy_index: *legacy_index,
                            handles: card_handles,
                            node_map: node_map.clone(),
                            group_preview_map: Vec::new(),
                            preview_kinds: Default::default(),
                            applied_graph_version: fx.graph_version,
                            bound,
                            user_bindings_version: fx.graph_version,
                            def_content_key: 0,
                            generator_input_node,
                            card_prefix: prefix.clone(),
                            relight_writes,
                        });
                    }
                    prev_node = output.0;
                    prev_out_port = output.1;
                    continue;
                }
                SpliceUnit::Card(k) => &active_effects[*k],
            };
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
                    let Some((mix_id, mix_out)) =
                        close_mix_group(&mut graph, &closing, (prev_node, prev_out_port))
                    else {
                        eprintln!(
                            "[chain-build-fail] close_mix_group failed mid-loop \
                             for group_id={:?}",
                            closing.group_id,
                        );
                        return None;
                    };
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

            // Look up the JSON-loaded view. Every shipping effect has
            // presetMetadata + bindings + skip_mode in its JSON file —
            // see `assets/effect-presets/`. The preflight above
            // guarantees the view exists.
            let Some(base_view) = loaded_preset_view_by_id(fx.effect_type()) else {
                eprintln!(
                    "[chain-build-fail] post-preflight view lookup for \
                     effect_type={:?} returned None — should be unreachable",
                    fx.effect_type(),
                );
                return None;
            };
            // Freeze compiler (design §12, step 2): fusion is on-demand and keyed
            // by the def's CONTENT, so ANY shape — shipped, edited in the node
            // editor, or created — fuses through one cache. The "effective def" is
            // the user's edited graph when present, else the canonical preset;
            // `fused_view_for` compiles-on-miss + caches by that def's content, so
            // an edited shape fuses on editor-close exactly like a shipped one
            // (and a freshly-warmed canonical hits the cache `tune_all` filled).
            // The gate (`should_render_fused`) only suppresses fusion while this
            // effect is the editor's *watched* target — kept unfused so node-output
            // preview can sample inner-node textures and edits render live. There
            // is no `has_override` veto: an override is just the effective def to
            // fuse. The fused view keeps the same outer-card params + skip mode, so
            // every line below (splice, outer_param_index, bindings) is shape-
            // identical. `fused_view_for` returns `None` for any shape with no
            // fusable region (or a binding that would strand) → renders unfused.
            let effective_def: &EffectGraphDef = fx.graph.as_ref().unwrap_or(&base_view.canonical_def);
            // D8/P7: relight now fuses. Augment with DEFAULT knob values before
            // the fusion compiler so the fused-view cache key (and generated
            // WGSL) is knob-invariant; the live values are written per-frame
            // via `EffectSlot::relight_writes`. `height_from` changes template
            // topology, so it legitimately recompiles — it is not folded into
            // the default-augmented key.
            let effective_def_for_fusion: std::borrow::Cow<'_, EffectGraphDef> = if fx.relight_active() {
                std::borrow::Cow::Owned(crate::node_graph::relight::relight_augment(
                    effective_def,
                    primitives,
                    &RelightParams::default(),
                ))
            } else {
                std::borrow::Cow::Borrowed(effective_def)
            };
            let fused_view: Option<std::sync::Arc<LoadedPresetView>> =
                if crate::node_graph::freeze::install::should_render_fused(
                    preview_effect == Some(&fx.id),
                ) {
                    crate::node_graph::freeze::install::fused_view_for(
                        &effective_def_for_fusion,
                        base_view,
                    )
                } else {
                    None
                };
            if fused_view.is_some() {
                // Step-7 attribution (minimal): one line per chain rebuild so the
                // operator can confirm a card is rendering through the fused
                // kernel. Rebuilds are editing-time events (topology change /
                // resize / editor close), not per-frame, so not hot-path spam.
                // Grep `[freeze]`.
                eprintln!(
                    "[freeze] {} → FUSED kernel (region collapsed to 1 dispatch)",
                    fx.effect_type().as_str()
                );
            }
            let view: &LoadedPresetView = fused_view.as_deref().unwrap_or(base_view);
            if is_skipped_for(view.skip_mode, &view.type_id, fx) {
                // No workers added — previous output flows directly
                // to the next effect.
                continue;
            }
            // The def actually spliced into the chain:
            //   - fused  → the fused def (already contains the relight template
            //              with default params if the toggle is on; live values
            //              are pushed per-frame via `relight_writes`);
            //   - else, an edited override → the user's wiring, materialized
            //              directly (the watched / non-fusable case);
            //   - else   → the canonical preset.
            // On primary-splice failure, fall back to the canonical (unfused) def —
            // it always builds — recording a divergent error when we were trying
            // the user's edited graph or a fused kernel.
            let splice_def: &EffectGraphDef = if fused_view.is_some() {
                &view.canonical_def
            } else if let Some(def) = &fx.graph {
                def
            } else {
                &view.canonical_def
            };
            // D8/P7: when the card is fused, the relight template is already
            // folded into `splice_def`; do NOT re-augment. On the unfused path
            // the template is spliced here with the live knob values.
            let relight_params = if fused_view.is_some() {
                None
            } else {
                fx.relight_active().then_some(&fx.relight_params)
            };
            let splice_result = match splice_def_into_chain(
                &mut graph,
                (prev_node, prev_out_port),
                splice_def,
                primitives,
                relight_params,
            ) {
                Some(r) => r,
                None => {
                    if fx.graph.is_some() || fused_view.is_some() {
                        record_chain_error(
                            &mut errors,
                            ChainError::DivergentGraphFellBack {
                                effect_id: fx.id.clone(),
                                effect_type: fx.effect_type().clone(),
                            },
                        );
                    }
                    match splice_def_into_chain(
                        &mut graph,
                        (prev_node, prev_out_port),
                        &base_view.canonical_def,
                        primitives,
                        relight_params,
                    ) {
                        Some(r) => r,
                        None => {
                            eprintln!(
                                "[chain-build-fail] canonical splice failed for \
                                 effect_type={:?} (effect_id={:?}) after fallback",
                                fx.effect_type(),
                                fx.id,
                            );
                            return None;
                        }
                    }
                }
            };
            let SpliceResult {
                output,
                handles,
                generator_input_id,
            } = splice_result;
            // Pair each spliced runtime node with its stable NodeId — the
            // binding resolution map. Built from the splice's handle list
            // (one entry per effect-local node) by reading each node's
            // `node_id` off the live graph. Node ids are unique, so this
            // is an unambiguous NodeId → NodeInstanceId map. Bindings
            // resolve against it, not the handle list, so grouping a node
            // (which prefixes its handle) leaves bindings intact.
            let node_map: Vec<(NodeId, NodeInstanceId)> = handles
                .iter()
                .filter_map(|(_, id)| graph.get_node(*id).map(|inst| (inst.node_id.clone(), *id)))
                .collect();
            // String outer-card bindings for this effect, resolved against its
            // splice node_map (parity with the generator path's
            // `string_bindings`). A String param can't ride the float `apply`
            // loop, so these are applied separately (defaults seeded at build).
            // No shipping effect declares any today (Vec stays empty), so this
            // is inert until one does. `flat_splice_def` feeds each binding's
            // `def_value` seed (BUG-182) and the preview-kind propagation below.
            let flat_splice_def = manifold_core::flatten::flatten_groups(splice_def).ok();
            if let Some(meta) = view.canonical_def.preset_metadata.as_ref() {
                for b in &meta.string_bindings {
                    if let manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } =
                        &b.target
                        && let Some((_, inst_id)) =
                            node_map.iter().find(|(nid, _)| nid == node_id)
                    {
                        chain_string_bindings.push(StringBindingResolution {
                            target_node: *inst_id,
                            target_param: param.clone(),
                            source_key: b.id.clone(),
                            default: b.default_value.clone(),
                            def_value: flat_splice_def
                                .as_ref()
                                .and_then(|flat| def_string_param_value(flat, node_id, param)),
                        });
                    }
                }
            }
            // Group → producer map for the node-output preview, derived from
            // the same pre-flatten def that was just spliced (`splice_def`) so
            // its group containers and producers carry the same stable NodeIds
            // as `node_map`. The spliced graph itself has no group nodes left to
            // resolve a selected collapsed group against. Empty for groupless
            // defs, which is nearly all of them. The watched (preview) effect is
            // always unfused, so `splice_def` is the user's edited graph when
            // present, else the canonical preset — either way the def whose
            // groups the editor is showing.
            let group_preview_map =
                manifold_core::flatten::group_output_producer_map(splice_def);
            // Propagated per-node preview kind, computed from the flattened def
            // (groups inlined) so a filter inherits the data kind of whatever
            // upstream node feeds it. `node_id`s survive flatten (nodeId-safety
            // invariant), so these keys match `node_map` / `group_preview_map`.
            let preview_kinds = flat_splice_def
                .as_ref()
                .map(crate::node_graph::PreviewEncoding::propagate)
                .unwrap_or_default();
            // Build the unified resolved-binding list: static prefix
            // first (view.bindings → ResolvedBinding::from_static),
            // then user tail (per-instance UserParamBinding →
            // ResolvedBinding::from_user). Each binding reads its value from
            // `PresetInstance.params` by `source_id` (the binding's own id),
            // so a single outer slider can fan out to multiple inner-node
            // params by sharing a source_id and order never matters.
            // User-added bindings now live in the per-instance graph's
            // `preset_metadata` (the single binding-storage list);
            // `user_param_bindings()` synthesizes the runtime view (routing
            // from the binding + range from its reshape note).
            let user_bindings = fx.user_param_bindings();
            // Per-instance reshape override. A reshape on a STOCK binding
            // (`scale`/`offset` on the card binding) lives on the EFFECTIVE def
            // — the user's edited `fx.graph` — not on the canonical
            // `view.bindings` the static prefix iterates (and a fused view's
            // bindings were retargeted from the CANONICAL def, so they'd drop
            // it too). Read the effective scale/offset per binding id here so
            // the reshape reaches the inner node on BOTH the fused and unfused
            // paths, keyed by id (the fuse retarget preserves binding ids).
            // Empty — no clone, no override, byte-identical — for an un-edited
            // instance, which is the overwhelming majority.
            let reshape_override: ahash::AHashMap<&str, (f32, f32)> = fx
                .graph
                .as_ref()
                .and_then(|g| g.preset_metadata.as_ref())
                .map(|m| {
                    m.bindings
                        .iter()
                        .map(|b| (b.id.as_str(), (b.scale, b.offset)))
                        .collect()
                })
                .unwrap_or_default();
            let mut bindings: Vec<ResolvedBinding> =
                Vec::with_capacity(view.bindings.len() + user_bindings.len());
            for b in view.bindings.iter() {
                // Reshape (range/curve/invert + scale/offset) is read from the
                // preset spec carried on `b` (ParamBinding), with the
                // effective def's scale/offset patched in when this instance
                // carries a per-instance reshape. The clone reuses `b`'s
                // `&'static` label/param pointers, so the override never leaks.
                let patched;
                let b = match reshape_override.get(b.id.as_ref()) {
                    Some(&(scale, offset)) if (scale, offset) != (b.scale, b.offset) => {
                        patched = ParamBinding {
                            scale,
                            offset,
                            ..b.clone()
                        };
                        &patched
                    }
                    _ => b,
                };
                match ResolvedBinding::from_static(b, &node_map) {
                    Some(rb) => {
                        bindings.push(rb);
                    }
                    None => record_chain_error(
                        &mut errors,
                        ChainError::StaticBindingHandleMissing {
                            effect_type: view.type_id.clone(),
                            binding_id: b.id.to_string(),
                        },
                    ),
                }
            }
            for core in user_bindings.iter() {
                // When this effect renders FUSED, the inner node this user
                // binding targets was collapsed into a fused kernel, so its
                // stable `node_id` no longer resolves against `node_map`.
                // Repoint the binding onto the fused node's uniform field
                // (`n{idx}_<param>`) via the view's retarget map — the same
                // retarget the static card bindings already went through.
                // The map is empty on an unfused view (the live editor path),
                // so this is a no-op clone-free lookup there. Without this,
                // a user-exposed slider silently goes inert the moment the
                // effect re-fuses on editor close.
                let retargeted = view
                    .fused_retarget
                    .get(&(core.node_id.as_str().to_string(), core.inner_param.clone()))
                    .map(|(fused_id, field)| {
                        let mut c = core.clone();
                        c.node_id = fused_id.clone();
                        c.inner_param = field.clone();
                        // The fused uniform field consumes Float, not Enum —
                        // same convert rewrite the static retarget applies.
                        c.convert =
                            crate::node_graph::freeze::install::convert_for_fused_field(c.convert);
                        c
                    });
                let resolve_core = retargeted.as_ref().unwrap_or(core);
                match ResolvedBinding::from_user(resolve_core, &graph, &node_map) {
                    Some(rb) => bindings.push(rb),
                    None => record_chain_error(
                        &mut errors,
                        ChainError::UserBindingResolveFailed {
                            effect_id: fx.id.clone(),
                            effect_type: fx.effect_type().clone(),
                            binding_id: core.id.clone(),
                            node_id: core.node_id.to_string(),
                            inner_param: core.inner_param.clone(),
                            rehydrate: false,
                        },
                    ),
                }
            }
            // Hand the resolved binding list to the shared `BoundGraph`, which
            // seeds the skip cache and plants each binding's declared default into
            // its inner target (so the per-frame skip-on-unchanged check holds
            // against an inner that already matches — closes both the static
            // "touch to update" bug (518436a7) and the symmetric user-binding
            // default-seed gap).
            let mut bound = BoundGraph::new(bindings, &mut graph);
            // Carry the fused view's retarget so an in-place inner-param edit /
            // undo on a fused-away node reaches the live kernel (BUG-006). Empty
            // clone on an unfused view — the live editor path — so no cost there.
            bound.fused_retarget = view.fused_retarget.clone();
            // The user tail lives in the graph now, so its rebuild signal
            // is the graph version (a binding add/remove/reshape bumps it).
            let user_bindings_version = fx.graph_version;
            let relight_writes = build_relight_writes(
                fx.relight_active(),
                &handles,
                &node_map,
                &bound.fused_retarget,
                "",
            );
            effect_nodes.push(EffectSlot {
                effect_id: fx.id.clone(),
                effect_type: fx.effect_type().clone(),
                legacy_index: *legacy_index,
                handles,
                node_map,
                group_preview_map,
                preview_kinds,
                applied_graph_version: fx.graph_version,
                bound,
                user_bindings_version,
                def_content_key: crate::node_graph::freeze::install::def_content_key(
                    effective_def,
                ),
                generator_input_node: generator_input_id,
                card_prefix: String::new(),
                relight_writes,
            });
            prev_node = output.0;
            prev_out_port = output.1;
        }

        // Close any still-open partial-wet-dry group at chain end.
        if let Some(closing) = open_group.take() {
            let Some((mix_id, mix_out)) =
                close_mix_group(&mut graph, &closing, (prev_node, prev_out_port))
            else {
                eprintln!(
                    "[chain-build-fail] close_mix_group failed for \
                     group_id={:?} at chain end",
                    closing.group_id,
                );
                return None;
            };
            group_mix_nodes.push((closing.group_id.clone(), mix_id));
            prev_node = mix_id;
            prev_out_port = mix_out;
        }

        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        if let Err(e) = graph.connect((prev_node, prev_out_port), (final_out, "in")) {
            eprintln!(
                "[chain-build-fail] final_out connect failed: {e:?} \
                 (last effect = {:?}.{:?})",
                prev_node, prev_out_port,
            );
            return None;
        }

        // Compile and find the resources we need to pin / read.
        let plan = match compile(&graph) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "[chain-build-fail] compile(&chain_graph) failed: {e:?} — \
                     chain returns None, layer falls back to source passthrough. \
                     Active effects: {:?}",
                    active_effects
                        .iter()
                        .map(|(_, fx)| fx.effect_type())
                        .collect::<Vec<_>>(),
                );
                return None;
            }
        };
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
        let assignment = assign_texture2d_slots(&plan, source_resource, (width, height));

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
            let (slot_w, slot_h) = assignment.slot_dims[slot_idx as usize];
            let rt = if let Some(p) = pool {
                RenderTarget::new_pooled(p, slot_w, slot_h, GRAPH_FORMAT, label)
            } else {
                RenderTarget::new(device, slot_w, slot_h, GRAPH_FORMAT, label)
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

        // Pre-allocate every Array<T> buffer + Texture3D volume the
        // plan declares, then run the post-allocation audit. Routed
        // through the canonical `graph_loader::pre_allocate_resources`
        // so the chain graph and JSON generators share one pipeline —
        // any feature added on either side applies to both. Replaces
        // the effect-only `pre_allocate_array_buffers_effect` shim that
        // shipped with commit 3500e7a7 and lacked Texture3D + audit
        // coverage.
        if let Err(e) =
            crate::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend)
        {
            record_chain_error(
                &mut errors,
                ChainError::PreAllocationFailed {
                    reason: e.to_string(),
                },
            );
            // Pre-allocation failure is fatal for the chain — we still
            // return None, but the structured error is now reachable
            // by any host that wants to surface it (today via the
            // terminal `[chain-error]` log; tomorrow the editor).
            return None;
        }

        let topology_hash = compute_topology_hash(effects, groups, width, height, preview_effect);

        let seeded_forced_epoch = graph.forced_outputs_epoch();
        let mut runtime = Self {
            graph,
            plan,
            last_forced_outputs_epoch: seeded_forced_epoch,
            executor: Executor::new(Box::new(backend)),
            effect_nodes,
            group_mix_nodes,
            io: PresetIo::Transform {
                source_slot,
                output_slot,
            },
            width,
            height,
            topology_hash,
            built_generation: crate::preset_loader::catalog_generation(),
            pending_segments,
            built_segment_generation: crate::node_graph::freeze::install::segment_generation(),
            state_store: StateStore::new(),
            errors,
            preview_encoding: crate::node_graph::PreviewEncoding::default(),
            type_id: None,
            target_format: None,
            string_bindings: chain_string_bindings,
        };
        // Seed each String binding's declared default into its inner node, the
        // same one-shot the generator path does at construction (a no-op when
        // no effect in the chain declares any).
        runtime.apply_string_defaults();
        if let Some(prior) = prior {
            runtime.harvest_state_from(prior);
        }
        Some(runtime)
    }

    /// State harvest across a rebuild (docs/CHAIN_FUSION_DESIGN.md §5): for
    /// every card whose `(effect_id, def_content_key)` matches a card in the
    /// prior runtime, move the prior node *impls* (the `Box<dyn EffectNode>`
    /// holding sim buffers, trail textures, DNN workers) and their StateStore
    /// buckets into this runtime, matched per node by stable `NodeId` + type.
    ///
    /// Safe because a matching content key means the prior impl's baked
    /// configuration (ports, WGSL source, pipelines — all derived from the
    /// def) is identical to what this build just constructed; params and
    /// bindings live on `NodeInstance` / the binding apply path and are this
    /// build's own. Skipped when dimensions changed — resolution-dependent
    /// state must rebuild, exactly as today. Cards that were edited (key
    /// mismatch) or removed keep fresh instances; intentional resets (seek,
    /// project load, idle clear, card deletion) run through `clear_state` /
    /// pool eviction, untouched by this path.
    fn harvest_state_from(&mut self, prior: &mut Self) {
        if prior.width != self.width || prior.height != self.height {
            return;
        }
        // Harvest only when the chain is the SAME SET of active cards —
        // reorders, value edits, editor open/close, fused-segment swap-ins.
        // A membership change (card added / removed / enabled / disabled /
        // skip-flipped) resets everything, exactly as before the harvest
        // existed: a feedback trail accumulated through a card that was just
        // toggled off holds that card's look — and latching blends (Screen /
        // Additive at full amount) would hold it FOREVER, leaving stale
        // blown-out frames rotating in the loop with no escape. Toggling is
        // an intentional look change; the reset is the escape hatch.
        let same_card_set = self.effect_nodes.len() == prior.effect_nodes.len()
            && self
                .effect_nodes
                .iter()
                .all(|s| prior.effect_nodes.iter().any(|p| p.effect_id == s.effect_id));
        if !same_card_set {
            return;
        }
        // new instance → old instance, for every node whose impl was carried
        // over. Drives the persistent-texture pass below.
        let mut harvested: ahash::AHashMap<NodeInstanceId, NodeInstanceId> =
            ahash::AHashMap::default();
        for (idx, slot) in self.effect_nodes.iter().enumerate() {
            if slot.def_content_key == 0 {
                continue;
            }
            let Some((old_idx, old_slot)) =
                prior.effect_nodes.iter().enumerate().find(|(_, s)| {
                    s.effect_id == slot.effect_id
                        && s.effect_type == slot.effect_type
                        && s.def_content_key == slot.def_content_key
                })
            else {
                continue;
            };
            // A stateful card's state is a function of what FEEDS it — a
            // feedback trail is a picture of the upstream chain. Carry it
            // only when the ordered sequence of cards before this one is
            // unchanged; an upstream reorder resets exactly this card (the
            // trail's content no longer corresponds to anything the chain
            // produces, and latching blends would hold the stale look
            // forever). Downstream reorders carry. Identity is by EffectId,
            // not content key, so upstream VALUE edits still carry — the
            // trail just evolves with the new look.
            let prefix_unchanged = idx == old_idx
                && self.effect_nodes[..idx]
                    .iter()
                    .zip(&prior.effect_nodes[..old_idx])
                    .all(|(a, b)| a.effect_id == b.effect_id);
            if !prefix_unchanged {
                continue;
            }
            for (node_id, new_inst) in &slot.node_map {
                let Some((_, old_inst)) =
                    old_slot.node_map.iter().find(|(nid, _)| nid == node_id)
                else {
                    continue;
                };
                let Some(old_node) = prior.graph.get_node_mut(*old_inst) else {
                    continue;
                };
                let Some(new_node) = self.graph.get_node_mut(*new_inst) else {
                    continue;
                };
                if old_node.node.type_id() != new_node.node.type_id() {
                    continue;
                }
                std::mem::swap(&mut old_node.node, &mut new_node.node);
                prior
                    .state_store
                    .migrate_node(*old_inst, *new_inst, &mut self.state_store);
                harvested.insert(*new_inst, *old_inst);
            }
        }
        if std::env::var("MANIFOLD_LOG_HARVEST").is_ok() {
            eprintln!(
                "[harvest] slots new={} prior={} nodes_carried={} persistent_new={}",
                self.effect_nodes.len(),
                prior.effect_nodes.len(),
                harvested.len(),
                self.plan.persistent_resources().len(),
            );
        }
        if harvested.is_empty() {
            return;
        }

        // Cross-frame PIXELS live in backend persistent slots, not in the
        // impls or the StateStore — feedback's trail is its persistent `out`
        // texture, and the back-edge producer's slot is the other half of
        // the zero-copy ping-pong (`FeedbackState` tracks only dims + mode).
        // Install each harvested node's persistent textures into the new
        // backend's slots: one atomic retain per texture, no GPU copy. The
        // migrated `FeedbackState` then correctly skips its first-frame
        // re-seed, reading the carried trail. (Array-buffer state —
        // `aliased_array_io` — is not migrated; no chain effect uses it.)
        let producer_of = |plan: &ExecutionPlan, node: NodeInstanceId, port: &str| {
            plan.steps().iter().find(|s| s.node == node).and_then(|s| {
                s.outputs
                    .iter()
                    .find(|(name, _)| *name == port)
                    .map(|(_, id)| *id)
            })
        };
        // (new persistent resource, old persistent resource) pairs.
        let mut moves: Vec<(ResourceId, ResourceId)> = Vec::new();
        for &res in self.plan.persistent_resources() {
            // Producing (node, port) of this persistent resource in the new plan.
            let Some((n_inst, port)) = self.plan.steps().iter().find_map(|s| {
                s.outputs
                    .iter()
                    .find(|(_, id)| *id == res)
                    .map(|(name, _)| (s.node, *name))
            }) else {
                continue;
            };
            let Some(&o_inst) = harvested.get(&n_inst) else {
                continue;
            };
            let Some(o_res) = producer_of(&prior.plan, o_inst, port) else {
                continue;
            };
            moves.push((res, o_res));
        }
        if moves.is_empty() {
            return;
        }
        // MOVE the owned RenderTargets across backends. Ownership (and pool
        // bookkeeping) transfers with the target; the prior runtime is being
        // dropped, so its emptied slots never render again. NEVER install via
        // `replace_texture_2d` here — that records a borrowed SHADOW over the
        // slot, and the feedback ping-pong's `swap_texture_2d` refuses
        // shadowed slots, freezing the trail with per-frame swap errors.
        let Some(old_metal) = prior
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
        else {
            return; // mock backend — nothing to move
        };
        let Some(new_metal) = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
        else {
            return;
        };
        let mut installed = 0usize;
        let total = moves.len();
        let mut installed_res: Vec<ResourceId> = Vec::with_capacity(total);
        for (res, o_res) in moves {
            let Some(old_slot) = crate::node_graph::Backend::slot_for(old_metal, o_res) else {
                continue;
            };
            let Some(new_slot) = crate::node_graph::Backend::slot_for(new_metal, res) else {
                continue;
            };
            let Some(rt) = old_metal.take_render_target(old_slot) else {
                continue;
            };
            // The displaced fresh target drops here (pooled → returns to pool).
            let _fresh = new_metal.swap_texture_2d(new_slot, rt);
            installed += 1;
            installed_res.push(res);
        }
        // The fresh executor would clear-to-black each persistent slot on
        // first acquisition — mark the carried ones initialized instead.
        for res in installed_res {
            self.executor.mark_persistent_initialized(res);
        }
        if std::env::var("MANIFOLD_LOG_HARVEST").is_ok() {
            eprintln!("[harvest] persistent targets moved {installed}/{total}");
        }
    }

    /// Structured errors produced by `try_build` and per-frame `run`.
    /// Each entry carries the affected effect's identity so the
    /// editor can attach the error to the right card. Empty when the
    /// chain built and ran cleanly.
    pub fn errors(&self) -> &[ChainError] {
        &self.errors
    }

    /// Preset catalog generation this chain was built against (step 10).
    /// The dispatcher compares it to the live catalog generation to decide
    /// whether a hot-reloaded preset edit requires a rebuild.
    pub fn built_generation(&self) -> u64 {
        self.built_generation
    }

    /// Whether this chain is waiting on a background segment compile, and the
    /// segment generation it was built at. The dispatcher rebuilds a waiting
    /// chain when the live generation has advanced (a worker result landed) —
    /// the fused-segment swap-in. Both no-ops once every segment resolved.
    pub fn awaiting_segment_swap(&self) -> bool {
        self.pending_segments
            && crate::node_graph::freeze::install::segment_generation()
                != self.built_segment_generation
    }

    /// Compare a cached graph's topology hash to the current chain's.
    /// Returns `true` if the same graph can be reused this frame.
    pub fn is_compatible(
        &self,
        effects: &[PresetInstance],
        groups: &[EffectGroup],
        width: u32,
        height: u32,
        preview_effect: Option<&EffectId>,
    ) -> bool {
        self.width == width
            && self.height == height
            && self.topology_hash
                == compute_topology_hash(effects, groups, width, height, preview_effect)
    }

    /// Run the cached chain graph against the upstream input texture.
    /// Returns a reference to the chain's output texture, or `None`
    /// if the executor couldn't be set up (should be unreachable in
    /// production).
    pub fn run(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        input_texture: &GpuTexture,
        effects: &[PresetInstance],
        groups: &[EffectGroup],
        ctx: &PresetContext,
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
            // Value-only / position-only graph edits bump `graph_version` but
            // NOT `graph_structure_version`, so the chain wasn't rebuilt. Push
            // the new inner-node param values into the live graph in place —
            // primitive state (feedback, sims) is preserved because nothing was
            // torn down. Only runs on the frame an edit lands, not per frame.
            // Clearing the binding cache makes the `apply_bindings` call below
            // re-assert every bound (live-driven) param over what we just set,
            // so bound params keep their live value and only the unbound
            // inner-node values change.
            if fx.graph_version != slot.applied_graph_version {
                // `slot.card_prefix` translates `fx.graph`'s (unprefixed,
                // per-card) node ids into the segment's `c{i}.`-prefixed
                // `node_map`/`fused_retarget` namespace for a segment member
                // (BUG-111); `""` for a whole-card slot, a no-op lookup.
                slot.bound.apply_inner_overrides_prefixed(
                    &mut self.graph,
                    &slot.node_map,
                    fx.graph.as_ref(),
                    &slot.card_prefix,
                );
                slot.applied_graph_version = fx.graph_version;
            }
            // Re-hydrate the user tail if the effect's binding list
            // moved since this slot was built. Exposing or unexposing
            // a param bumps the core-side version without rebuilding
            // the chain — without this catch, a freshly-exposed
            // outer slot writes into an empty runtime list and
            // never reaches the inner node (the symptom: exposed
            // slider has no effect, while the same value set in the
            // graph editor works).
            if fx.graph_version != slot.user_bindings_version {
                // Static/user boundary is derived, not stored: the static
                // prefix is contiguous and comes first, so its count is the
                // number of `Static`-sourced bindings. Truncating to it drops
                // the stale user tail before we rebuild it.
                let n_static = slot
                    .bound
                    .bindings
                    .iter()
                    .filter(|b| matches!(b.source, BindingSource::Static))
                    .count();
                slot.bound.bindings.truncate(n_static);
                for core in fx.user_param_bindings().iter() {
                    match ResolvedBinding::from_user(core, &self.graph, &slot.node_map) {
                        Some(rb) => slot.bound.bindings.push(rb),
                        None => record_chain_error(
                            &mut self.errors,
                            ChainError::UserBindingResolveFailed {
                                effect_id: fx.id.clone(),
                                effect_type: slot.effect_type.clone(),
                                binding_id: core.id.clone(),
                                node_id: core.node_id.to_string(),
                                inner_param: core.inner_param.clone(),
                                rehydrate: true,
                            },
                        ),
                    }
                }
                // Plant the new tail's declared defaults into the inner
                // targets so the cache's "Applied(default)" claim holds
                // against an inner that already matches — symmetric
                // with the static-prefix seed at chain-build time.
                apply_binding_defaults(&slot.bound.bindings[n_static..], &mut self.graph, None);
                slot.user_bindings_version = fx.graph_version;
                // Reset the user-tail cache so the first apply after
                // re-hydrate unconditionally writes — the previous
                // cache entries refer to a different binding list and
                // would skip-write on stale-prev compare.
                slot.bound.cache.clear_tail(n_static);
            }
            slot.bound.apply(&mut self.graph, &fx.params);
            // Push the "3D Shading" D3 relight knobs into the spliced graph
            // every frame. Float-knob edits are no longer structural (D8/P7),
            // so the chain doesn't rebuild on a drag; these writes keep the
            // live values reaching the template nodes (unfused) or the fused
            // kernel's uniform fields (fused).
            slot.apply_relight_params(&mut self.graph, &fx.relight_params);
            // If the preset includes a `system.generator_input` node,
            // push every frame-context scalar (time / beat / aspect /
            // output dims / trigger_count / anim_progress) into its
            // params. The standard port-shadows-param machinery
            // propagates these to inner primitives via scalar wires —
            // same surface generators have, no per-effect Rust code.
            // `trigger_count` used to stay pinned at
            // the primitive's 0.0 default here ("clip-side concepts that
            // don't reach the effect chain") — the caller now feeds the
            // owning layer's EFFECTIVE count (clip edge + audio fires,
            // §8 D1) same as a generator graph gets; master/global chains
            // have no layer, so their clip contribution is 0 and only
            // audio fires move the count. `anim_progress` stays clip-side
            // (effects have no anim_progress concept) and is always 0.0
            // for effect chains — only `trigger_count` changed.
            if let Some(node) = slot.generator_input_node {
                let aspect = if ctx.height > 0 {
                    ctx.width as f32 / ctx.height as f32
                } else {
                    1.0
                };
                let _ = self
                    .graph
                    .set_param(node, "time", ParamValue::Float(ctx.time as f32));
                let _ = self
                    .graph
                    .set_param(node, "beat", ParamValue::Float(ctx.beat as f32));
                let _ = self.graph.set_param(node, "aspect", ParamValue::Float(aspect));
                let _ = self.graph.set_param(
                    node,
                    "trigger_count",
                    ParamValue::Float(ctx.trigger_count as f32),
                );
                let _ = self.graph.set_param(
                    node,
                    "output_width",
                    ParamValue::Float(ctx.output_width as f32),
                );
                let _ = self.graph.set_param(
                    node,
                    "output_height",
                    ParamValue::Float(ctx.output_height as f32),
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
        let PresetIo::Transform {
            source_slot,
            output_slot,
        } = self.io
        else {
            // `run` is the effect-chain entry; a generator-IO runtime renders
            // via `render` instead. Defensive — callers never cross the wires.
            return None;
        };
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("PresetRuntime backend is MetalBackend");
        let ok = metal.replace_texture_2d(source_slot, input_texture.clone());
        debug_assert!(ok, "source slot pre-bound at build time");

        let frame_time = FrameTime {
            beats: manifold_core::Beats(ctx.beat),
            seconds: manifold_core::Seconds(ctx.time),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
            // Forward host frame counter so legacy effects (DoF /
            // WireframeDepth / BlobTracking) dispatched via the
            // PresetRuntime fast path can throttle correctly.
            frame_count: ctx.frame_count,
        };
        // Use the StateStore-aware execute path so stateful primitives
        // that key per-owner state off `(node_id, owner_key)` — today
        // only `temporal::Feedback`, but any future primitive using the
        // StateStore API — get the StateStore + owner_key they need.
        // The `with_gpu` variant passes `state: None, owner_key: 0`,
        // which makes those primitives panic.
        self.refresh_plan_if_forced_outputs_changed();
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
        self.executor.backend().texture_2d(output_slot)
    }

    /// The chain's final output texture from the most recent
    /// [`Self::run`]. Returns `None` if the backend lookup fails
    /// (should be unreachable since `output_slot` was pre-bound at
    /// build time), or if this is a generator-IO runtime (no owned output).
    pub fn output_texture(&self) -> Option<&GpuTexture> {
        let PresetIo::Transform { output_slot, .. } = self.io else {
            return None;
        };
        self.executor.backend().texture_2d(output_slot)
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

    /// Stable identity for the `GeneratorRegistry`. Panics on an effect-chain
    /// runtime (which has no single type id).
    pub fn type_id(&self) -> &PresetTypeId {
        self.type_id
            .as_ref()
            .expect("type_id() called on an effect-chain PresetRuntime")
    }

    /// Alias for [`Self::type_id`] kept for call-site readability on the
    /// generator path.
    pub fn generator_type(&self) -> &PresetTypeId {
        self.type_id()
    }

    /// Replace the internal executor — used when a host wires a real backend
    /// after a mock-backend construction.
    pub fn set_executor(&mut self, executor: Executor) {
        self.executor = executor;
    }

    /// Update the `system.generator_input` node's per-frame context. No-op on
    /// an effect-chain runtime.
    pub fn set_frame_context(&mut self, fc: FrameContextInputs) {
        let FrameContextInputs {
            time,
            beat,
            aspect,
            trigger_count,
            anim_progress,
            output_width,
            output_height,
        } = fc;
        let PresetIo::Generate {
            generator_input_id, ..
        } = self.io
        else {
            return;
        };
        let id = generator_input_id;
        let _ = self.graph.set_param(id, "time", ParamValue::Float(time));
        let _ = self.graph.set_param(id, "beat", ParamValue::Float(beat));
        let _ = self.graph.set_param(id, "aspect", ParamValue::Float(aspect));
        let _ = self
            .graph
            .set_param(id, "trigger_count", ParamValue::Float(trigger_count));
        let _ = self
            .graph
            .set_param(id, "anim_progress", ParamValue::Float(anim_progress));
        let _ = self
            .graph
            .set_param(id, "output_width", ParamValue::Float(output_width));
        let _ = self
            .graph
            .set_param(id, "output_height", ParamValue::Float(output_height));
    }

    /// Push the host's slider values through the preset's bindings to the
    /// matching inner-node params (generator path). Each binding reads its value
    /// from the id-keyed `params` manifest by `source_id`; an empty manifest
    /// leaves every binding at its declared default. No per-frame allocation —
    /// the manifest is borrowed directly, no float-bus wrapping.
    pub fn apply_param_values(&mut self, params: &ParamManifest) {
        if let Some(seg) = self.effect_nodes.first_mut() {
            seg.bound.apply(&mut self.graph, params);
        }
    }

    /// Any node in this graph with background file IO still in flight
    /// (`EffectNode::io_pending` — the IoBridge decode-thread sources).
    /// Headless convergence loops (`render-import`, conformance tests) call
    /// this after each rendered frame: while it returns `true`, byte-stable
    /// frames must NOT count toward convergence, because a source emitting
    /// stable black during a long decode (a 74 MB 4k EXR takes seconds) is
    /// indistinguishable from a settled frame by readback alone
    /// (GLB_CONFORMANCE_DESIGN.md G-P6 gate-review fix). Nodes pruned from
    /// the frame's dispatch (e.g. a mux's unselected branch) never spawn
    /// their decode, so they report `false` and can't wedge the loop.
    pub fn io_pending(&self) -> bool {
        self.graph.nodes().any(|n| n.node.io_pending())
    }

    /// Push a value/position editor edit's inner-node values into the running
    /// generator in place (no rebuild — sim/particle state survives) and clear
    /// the binding cache so live card sliders re-assert over them.
    pub fn apply_inner_param_overrides(
        &mut self,
        def: &manifold_core::effect_graph_def::EffectGraphDef,
    ) {
        if let Some(seg) = self.effect_nodes.first_mut() {
            // Disjoint borrows (same pattern as the chain's `run`):
            // seg.bound (mut) + seg.node_map (shared) + self.graph (mut).
            seg.bound
                .apply_inner_overrides(&mut self.graph, &seg.node_map, Some(def));
        }
    }

    /// BUG-317: recompile the plan when a live param write changed some
    /// node's forced-output set (see [`Graph::forced_outputs_epoch`]).
    /// Called after each frame's param refresh, before the executor runs —
    /// so a frame never executes against a plan whose `consumed_outputs`
    /// fold disagrees with the params the nodes will read. On compile
    /// failure the old plan is kept and the error logged (the graph is
    /// structurally unchanged, so failure here would indicate a bug, not
    /// bad user input — never panic on the live path).
    fn refresh_plan_if_forced_outputs_changed(&mut self) {
        let epoch = self.graph.forced_outputs_epoch();
        if epoch == self.last_forced_outputs_epoch {
            return;
        }
        self.last_forced_outputs_epoch = epoch;
        match compile(&self.graph) {
            Ok(p) => {
                log::info!(
                    "[preset-runtime] forced-outputs change (epoch {epoch}) — execution plan recompiled"
                );
                self.plan = p;
            }
            Err(e) => {
                log::error!(
                    "[preset-runtime] forced-outputs change (epoch {epoch}) but plan recompile failed: {e:?} — keeping previous plan"
                );
            }
        }
    }

    /// Run one frame against the configured executor (mock-backend test path).
    pub fn execute_frame(&mut self, time: FrameTime) {
        self.refresh_plan_if_forced_outputs_changed();
        self.executor
            .execute_frame(&mut self.graph, &self.plan, time);
    }

    /// Install the host-provided target texture as the source for
    /// `final_output.in` via `replace_texture_2d` (single atomic retain, no
    /// alloc). Panics if the backend isn't a `MetalBackend` (mock-backend mode
    /// never reaches this — `render` is the only caller).
    fn install_target(&mut self, target: &GpuTexture) {
        let slot = match self.io {
            PresetIo::Generate {
                final_output_slot: Some(slot),
                ..
            } => slot,
            _ => panic!(
                "PresetRuntime::install_target requires a Generate IO with a pre-bound \
                 final_output_slot — construct via from_def_with_device"
            ),
        };
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>())
            .expect(
                "PresetRuntime::install_target requires a MetalBackend — use \
                 from_def_with_device to construct the production path",
            );
        metal.replace_texture_2d(slot, target.clone());
    }

    /// Render one generator frame into the host-provided `target` texture.
    /// Returns the (passed-through) anim progress. `params` is the generator's
    /// id-keyed slider manifest (`Layer.gen_params.params`); pass
    /// [`ParamManifest::default`] (empty) to run every binding at its declared
    /// default (preview / import paths that don't override any card).
    pub fn render(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        target: &GpuTexture,
        ctx: &PresetContext,
        params: &ParamManifest,
    ) -> f32 {
        // 1. Push per-frame timing into the generator_input node's params.
        self.set_frame_context(FrameContextInputs {
            time: ctx.time as f32,
            beat: ctx.beat as f32,
            aspect: ctx.aspect,
            trigger_count: ctx.trigger_count as f32,
            anim_progress: ctx.anim_progress,
            output_width: ctx.output_width as f32,
            output_height: ctx.output_height as f32,
        });

        // 2. Push the host's outer-card slider values through the bindings.
        self.apply_param_values(params);

        // 3. Install the host's target as the FinalOutput's source slot.
        self.install_target(target);

        // 4. Run the graph through the state-aware executor entry.
        let frame_time = FrameTime {
            beats: Beats(ctx.beat),
            seconds: Seconds(ctx.time),
            delta: Seconds(ctx.dt as f64),
            frame_count: 0,
        };
        self.refresh_plan_if_forced_outputs_changed();
        self.executor.execute_frame_with_state(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
            &mut self.state_store,
            /* owner_key */ 0,
        );

        ctx.anim_progress
    }

    /// Reset all generator state (per-primitive `extra_fields` + the runtime
    /// `StateStore`). Called after export warmup re-seek.
    pub fn reset_state(&mut self, _device: &GpuDevice) {
        for inst in self.graph.nodes_mut() {
            inst.node.clear_state();
        }
        self.state_store.cleanup_all();
    }

    /// BUG-104 — release trigger-EDGE latch state ONLY (`EffectNode::
    /// is_trigger_latch` nodes: `sample_and_hold`, `clip_trigger_cycle`,
    /// `clip_trigger_index`, `frequency_ratio`, `cycle_table_row`,
    /// `trigger_gate`, `trigger_ease_to`), leaving every other primitive's
    /// persistent state (feedback textures, particle buffers, mip
    /// pyramids) untouched.
    ///
    /// The narrow sibling of [`Self::reset_state`]: generators are
    /// deliberately long-lived per layer (`docs/DECOMPOSING_GENERATORS.md`
    /// §9 — particle sims / feedback survive clip changes), so a full
    /// `reset_state()` on every transport stop would be its own
    /// regression (nuking sim state the performer expects to persist).
    /// A trigger latch has no such expectation — it exists only to mirror
    /// the "Trigger" card option's last edge, and once that option goes
    /// back to idle (or the transport stops / a project loads), the latch
    /// holding a stale captured value or cycle index with no way for the
    /// performer to release it IS the bug (BUG-104: "goes dead, and stays
    /// dead after Trigger is disabled"). Call from the same moments the
    /// playback-side `manifold_playback::modulation::clear_all_trigger_edges`
    /// already fires (transport stop, project load) — see
    /// `ContentThread::handle_command`'s `ContentCommand::Stop` /
    /// `ContentCommand::LoadProject` arms.
    pub fn clear_trigger_state(&mut self) {
        let mut latch_ids: Vec<NodeInstanceId> = Vec::new();
        for inst in self.graph.nodes_mut() {
            if inst.node.is_trigger_latch() {
                inst.node.clear_state();
                latch_ids.push(inst.id);
            }
        }
        self.state_store.cleanup_nodes(&latch_ids);
    }

    /// Resize the generator's backend + re-pre-bind the final-output
    /// placeholder + re-run the canonical pre-allocate pass (resize wipes every
    /// pinned binding incl. Array<T> buffers and Texture3D volumes).
    pub fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let Some(format) = self.target_format else {
            // Mock-backend test path — no GPU, no resources to invalidate.
            return;
        };
        let PresetIo::Generate {
            final_output_input_resource,
            ..
        } = self.io
        else {
            return;
        };
        let Some(metal) = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>())
        else {
            return;
        };
        metal.resize(width, height);
        // `resize` wiped every pinned binding (incl. the final-output
        // placeholder), so the slot index is stale. Pre-bind a fresh 1×1
        // placeholder; `install_target` swaps in the host's real target next
        // frame.
        let placeholder =
            RenderTarget::new(device, 1, 1, format, "preset_runtime_target_owner");
        let slot = metal.pre_bind_texture_2d(final_output_input_resource, placeholder);
        if let PresetIo::Generate {
            final_output_slot, ..
        } = &mut self.io
        {
            *final_output_slot = Some(slot);
        }
        // Re-run the canonical pre-allocate pass so downstream primitives don't
        // render against an empty Array<T>/Texture3D wire after resize.
        let Some(metal) = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>())
        else {
            return;
        };
        if let Err(e) =
            crate::node_graph::pre_allocate_resources(&self.graph, &self.plan, device, metal)
        {
            log::warn!("PresetRuntime::resize re-allocation failed: {e}");
        }
        // Resize wiped the Array<T> wire buffers, and stateful loops whose
        // state rides those buffers in place (`array_feedback`'s aliased
        // in/out variant — every particle sim) came back zeroed with no
        // re-seed: dead particles, black output, and no way for the
        // performer to recover short of rebuilding the layer. Clear ALL
        // graph state so every seed-bootstrap path re-arms — a resolution
        // change reads as "the sim restarts", which is the honest contract
        // (positions are UV-normalized but density/canvas-sized buffers
        // aren't resolution-portable anyway).
        for inst in self.graph.nodes_mut() {
            inst.node.clear_state();
        }
        self.state_store.cleanup_all();
    }

}
