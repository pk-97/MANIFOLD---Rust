//! [`ChainGraph`] — one cached [`Graph`] per `EffectChain`.
//!
//! Each chain compiles its full effect sequence (every active
//! [`PresetInstance`], plus `Mix` sub-graphs for wet/dry groups)
//! into a single graph runtime instance: one [`Graph`], one
//! [`ExecutionPlan`], one [`MetalBackend`], one [`Executor`]. That's
//! one ping/pong recycle pool for the chain, one executor step loop
//! per frame, one input-texture pre-bind per frame — no per-effect
//! dispatch overhead.
//!
//! Primitive state (mip pyramids, feedback buffers, depth workers)
//! lives inside the boxed [`EffectNode`] owned by the cached
//! [`Graph`]. Per-frame param changes refresh in place via
//! [`apply_bindings`]; topology changes (effect added /
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
//! - 1 `apply_bindings` call per effect (unified static + user tail)
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
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    BoundGraph, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, LoadedPresetView,
    MetalBackend, NodeInstanceId, ParamBinding, ParamValue, PrimitiveRegistry, ResolvedBinding,
    ResourceId, Slot, Source, SpliceResult, StateStore, apply_binding_defaults, compile,
    splice_def_into_chain,
};
use crate::node_graph::{is_skipped_for, loaded_preset_view_by_id};
use crate::preset_context::PresetContext;
use manifold_core::effect_graph_def::EffectGraphDef;
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
    /// Preset catalog generation this graph was built against (step 10
    /// hot-reload). The dispatcher compares it to the live
    /// [`crate::preset_loader::catalog_generation`] once per frame: when a
    /// preset `.json` is edited on disk the watcher bumps the generation,
    /// and any chain built against the old generation is rebuilt from the
    /// new defs. At rest the generation never moves, so the comparison is a
    /// single atomic load that always matches — no perform-path cost.
    built_generation: u64,
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
    /// Structured error log accumulated during `try_build` and the
    /// per-frame `run`. Each variant carries enough context
    /// (effect_id / effect_type / binding identity / node handle) for
    /// a future editor surface to attach the error to the affected
    /// effect card and inner node. Today this is consumed by tests
    /// and `errors()` for any host code that wants to surface them;
    /// the terminal log is the immediate user-visible benefit, written
    /// with the consistent `[chain-error]` prefix so logs grep
    /// cleanly.
    errors: Vec<ChainError>,
    /// How the currently-previewed node's output should be rendered (flow wheel
    /// / lift / raw), derived from its descriptor + port name when
    /// [`Self::set_preview_target`] resolves a target. `Color` when nothing on
    /// this chain is previewed. Read by the host via [`Self::preview_encoding`].
    preview_encoding: crate::node_graph::PreviewEncoding,
}

/// Structured error variants the chain runner produces. Every variant
/// carries the affected effect's identity so the future editor surface
/// can highlight the right card / node. Today this drives the
/// consistent `[chain-error]` terminal log; tomorrow it's the data
/// the editor reads via [`ChainGraph::errors`].
#[derive(Debug, Clone)]
pub enum ChainError {
    /// A per-instance divergent graph failed to splice; the chain
    /// fell back to the canonical preset. Most often caused by a
    /// stale handle reference after a primitive rename, or a
    /// type-id that no longer exists.
    DivergentGraphFellBack {
        effect_id: EffectId,
        effect_type: PresetTypeId,
    },
    /// A spec-level `ParamBinding` references a handle the splice
    /// didn't register. The binding silently doesn't apply — the
    /// outer-card slider exists but writes go nowhere. Usually the
    /// preset JSON's `bindings[].target.handle` was renamed without
    /// updating the inner node's handle.
    StaticBindingHandleMissing {
        effect_type: PresetTypeId,
        binding_id: String,
    },
    /// A user-exposed param binding (the editor's "expose to card")
    /// couldn't resolve. `rehydrate=false` means it failed at build
    /// time; `rehydrate=true` means it failed when the user toggled
    /// an exposure mid-show.
    UserBindingResolveFailed {
        effect_id: EffectId,
        effect_type: PresetTypeId,
        binding_id: String,
        node_id: String,
        inner_param: String,
        rehydrate: bool,
    },
    /// Pre-allocation failed for the whole chain — re-emitted from
    /// [`crate::node_graph::PreAllocationError`] so the chain-level
    /// error log carries it too. The chain build returned `None`
    /// and the operator sees the layer as a black passthrough.
    PreAllocationFailed { reason: String },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DivergentGraphFellBack {
                effect_id,
                effect_type,
            } => write!(
                f,
                "{} (id={}): divergent graph failed to splice — fell back to canonical preset",
                effect_type.as_str(),
                effect_id.as_str(),
            ),
            Self::StaticBindingHandleMissing {
                effect_type,
                binding_id,
            } => write!(
                f,
                "{}: ParamBinding `{}` references a handle the splice did not register; \
                 this binding will not apply",
                effect_type.as_str(),
                binding_id,
            ),
            Self::UserBindingResolveFailed {
                effect_type,
                binding_id,
                node_id,
                inner_param,
                rehydrate,
                ..
            } => {
                let when = if *rehydrate {
                    "on rehydrate"
                } else {
                    "at build time"
                };
                write!(
                    f,
                    "{}: UserParamBinding `{}` could not resolve {when} \
                     (node_id=`{}`, inner_param=`{}`); slider will not apply \
                     until the binding re-points to a live target",
                    effect_type.as_str(),
                    binding_id,
                    node_id,
                    inner_param,
                )
            }
            Self::PreAllocationFailed { reason } => write!(
                f,
                "resource pre-allocation failed: {reason}. Chain build returned None; \
                 operator will see the affected layer go black"
            ),
        }
    }
}

impl std::error::Error for ChainError {}

/// Push a [`ChainError`] onto an accumulator and emit one consistent
/// `[chain-error]` line. Replaces the scattered `eprintln!` calls that
/// previously lacked structure — same data lands in the log, plus
/// it's now reachable through [`ChainGraph::errors`] for the editor.
fn record_chain_error(errors: &mut Vec<ChainError>, err: ChainError) {
    eprintln!("[chain-error] {err}");
    errors.push(err);
}

struct EffectSlot {
    #[allow(dead_code)]
    effect_id: EffectId,
    effect_type: PresetTypeId,
    /// Index into the chain's `effects` slice at the time this slot
    /// was constructed. Topology rebuilds are triggered by any change
    /// to that slice's structural shape (effect added/removed/reordered,
    /// enabled-bit toggle, group enabled toggle, group crossing the
    /// 1.0 wet/dry boundary) — see [`compute_topology_hash`]. As long
    /// as the cached graph is reused, `effects[legacy_index]` is the
    /// same `PresetInstance` whose modulated `param_values` the
    /// renderer just updated.
    legacy_index: usize,
    /// Effect-local handles returned by `spec.splice` — names are
    /// scoped to this effect. `Cow<'static, str>` so canonical splices
    /// stay zero-allocation and user-edited divergent defs can hold
    /// owned strings off disk. Kept for state clearing (`clear_state`);
    /// binding resolution keys off [`Self::node_map`] instead.
    handles: Vec<(std::borrow::Cow<'static, str>, NodeInstanceId)>,
    /// `(NodeId, NodeInstanceId)` for every spliced node — the binding
    /// resolution map. Built once at chain build by pairing each
    /// spliced runtime node with its stable [`NodeId`]. Static and user
    /// bindings resolve their target `NodeId` against this (see
    /// [`ResolvedBinding::from_static`] / [`ResolvedBinding::from_user`]),
    /// so a binding survives the node's handle changing under grouping.
    node_map: Vec<(NodeId, NodeInstanceId)>,
    /// Group container `NodeId` → concrete inner producer `NodeId`, for the
    /// node-output preview. A group is a UI container that flattens away before
    /// the splice, so its own id is never in [`Self::node_map`]; selecting a
    /// collapsed group in the editor would otherwise preview nothing (black).
    /// Computed once at chain build from the pre-flatten preset def via
    /// [`manifold_core::flatten::group_output_producer_map`]; consulted by
    /// [`ChainGraph::set_preview_target`] to substitute the group's primary
    /// texture-output producer. The third element is that output's interface
    /// port name (`forceField`), which drives the preview encoding. Empty for
    /// groupless effects.
    group_preview_map: Vec<(NodeId, NodeId, String)>,
    /// Propagated preview data-kind per node `NodeId`, computed once at chain
    /// build from the flattened def via [`PreviewEncoding::propagate`]. Lets the
    /// node-output preview follow the *data*: a Gaussian Blur whose input was a
    /// force field resolves to `VectorField` here even though the blur's own
    /// descriptor says nothing. [`ChainGraph::set_preview_target`] looks the
    /// previewed node up here before falling back to single-node `derive`.
    preview_kinds: ahash::AHashMap<NodeId, crate::node_graph::PreviewEncoding>,
    /// Last `fx.graph_version` whose inner-node param overrides were pushed into
    /// the live graph. When the host's `graph_version` advances without a
    /// structure change (a value or position edit — no rebuild), `run` re-reads
    /// the def's node params and applies them in place via
    /// [`apply_inner_param_overrides`], so the edit lands without wiping
    /// primitive state. Seeded to `fx.graph_version` at build.
    applied_graph_version: u32,
    /// The card-driven binding lifecycle — the resolved binding list + the
    /// skip-on-unchanged cache — shared with the generator runtime via
    /// [`BoundGraph`]. `bound.bindings[0..n_static]` are spec bindings hydrated
    /// via [`ResolvedBinding::from_static`]; `bound.bindings[n_static..]` are
    /// user-exposed bindings hydrated via [`ResolvedBinding::from_user`]. Same
    /// order as `PresetInstance.param_values`, so [`apply_bindings`] walks both in
    /// lockstep against one cache.
    ///
    /// The user tail is re-hydrated lazily when
    /// `effect.user_param_bindings_version` advances past
    /// [`Self::user_bindings_version`]; the static prefix rebuilds on a
    /// `param_mappings_version` bump; the cache clears (via
    /// [`BoundGraph::apply_inner_overrides`]) on a `graph_version` bump.
    bound: BoundGraph,
    /// Boundary index inside [`Self::bindings`] separating the static
    /// prefix from the user tail. Equals the count of resolved static
    /// bindings — orphaned static bindings (handle missing from the
    /// splice map) are dropped before the slot is built, so this is
    /// the live length of `bindings[0..n_static]`.
    n_static: usize,
    /// Count of static *outer slots* on the host's
    /// `PresetInstance.param_values` — distinct from [`Self::n_static`]
    /// when an orphaned spec binding gets dropped at chain build.
    /// User tail bindings derive their `source_index` as
    /// `n_static_slots + j`, so that an orphaned static binding
    /// doesn't shift every subsequent user-tail slider down one slot.
    /// In the common case (no orphans) this equals `n_static`.
    n_static_slots: usize,
    /// Last seen `PresetInstance.graph_version` for the user tail. User
    /// bindings live in the per-instance graph now, so a binding add /
    /// remove / reshape bumps the graph version. When the live effect's
    /// version differs, the per-frame apply path re-hydrates the user tail
    /// of [`Self::bindings`] from the synthesized binding list before
    /// applying.
    user_bindings_version: u32,
    /// Effect-side `system.generator_input` node id, if the preset
    /// included one. Effects with a generator_input get per-frame
    /// scalars (time / beat / aspect / output dims) pushed to this
    /// node so the standard port-shadows-param wires propagate them
    /// to inner primitives — the same surface generators have.
    /// Lets effects react to project BPM, beat phase, output
    /// resolution, etc., without any per-effect Rust code.
    ///
    /// Every shipping effect that needs frame-context scalars uses this
    /// surface now (Glitch / Strobe / Watercolor / VoronoiPrism
    /// migrated 2026-05-28; see commits below). The previous
    /// `ctx_target_node` field + `apply_ctx_params_at` hardcoded match
    /// were deleted in the same change.
    generator_input_node: Option<NodeInstanceId>,
    /// The static spec bindings (clone of the preset's `ParamBinding`s)
    /// paired with each binding's `source_index`, retained so the static
    /// prefix can be rebuilt in place when a per-instance reshape note
    /// changes — without a full chain/graph rebuild. Resolution is
    /// deterministic against the stable `node_map`, so a rebuild
    /// reproduces the same `n_static` length.
    static_specs: Vec<(ParamBinding, usize)>,
    /// Last seen `PresetInstance.param_mappings_version`. When the live
    /// effect's version differs, the per-frame apply path rebuilds the
    /// static prefix of [`Self::bindings`] from [`Self::static_specs`] +
    /// the current notes before applying — the note analogue of the
    /// `user_bindings_version` user-tail rehydrate.
    param_mappings_version: u32,
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
        effects: &[PresetInstance],
        groups: &[EffectGroup],
        primitives: &PrimitiveRegistry,
        device: &GpuDevice,
        pool: Option<&TexturePool>,
        width: u32,
        height: u32,
        // Effect the graph editor is watching, if any. Its chain is built
        // unfused even when the freeze toggle is on, so its intermediate
        // node outputs exist for the authoring-time preview to sample. `None`
        // (no editor / not watching) leaves the freeze gate untouched — zero
        // effect on the live render path.
        preview_effect: Option<&EffectId>,
    ) -> Option<Self> {
        // Indexed so we can capture each active effect's original
        // position in `effects` — used as a per-frame O(1) lookup key
        // (replaces the previous AHashMap<EffectId, &PresetInstance>
        // rebuild). Topology changes rebuild this graph, so the
        // captured indices stay valid for the cache's lifetime.
        let active_effects: Vec<(usize, &PresetInstance)> = effects
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

        // Preflight: every active effect must have a `LoadedPresetView`
        // (JSON-loaded preset metadata). The chain build loop reads
        // bindings, skip mode, and the canonical splice all off the
        // view; an effect without one is unrunnable.
        for (_, fx) in &active_effects {
            if loaded_preset_view_by_id(fx.effect_type()).is_none() {
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
        // Structured error accumulator. Moves into `ChainGraph::errors`
        // at the end of this function; mid-build failures push here so
        // the editor (and the consistent `[chain-error]` terminal log)
        // can show them tied to the affected effect.
        let mut errors: Vec<ChainError> = Vec::new();

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
            let effective_def: &EffectGraphDef = fx.graph.as_ref().unwrap_or(base_view.canonical_def);
            let fused_view: Option<&LoadedPresetView> =
                if crate::node_graph::freeze::install::should_render_fused(
                    crate::node_graph::freeze::install::FuseTarget::Effect(fx.effect_type()),
                    preview_effect == Some(&fx.id),
                ) {
                    crate::node_graph::freeze::install::fused_view_for(effective_def, base_view)
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
            let view = fused_view.unwrap_or(base_view);
            if is_skipped_for(view.skip_mode, &view.type_id, fx) {
                // No workers added — previous output flows directly
                // to the next effect.
                continue;
            }
            // The def actually spliced into the chain:
            //   - fused  → the fused def (it already folds the effective shape's
            //              content, canonical or edited, into one kernel);
            //   - else, an edited override → the user's wiring, materialized
            //              directly (the watched / non-fusable case);
            //   - else   → the canonical preset.
            // On primary-splice failure, fall back to the canonical (unfused) def —
            // it always builds — recording a divergent error when we were trying
            // the user's edited graph or a fused kernel.
            let splice_def: &EffectGraphDef = if fused_view.is_some() {
                view.canonical_def
            } else if let Some(def) = &fx.graph {
                def
            } else {
                view.canonical_def
            };
            let splice_result = match splice_def_into_chain(
                &mut graph,
                (prev_node, prev_out_port),
                splice_def,
                primitives,
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
                        base_view.canonical_def,
                        primitives,
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
            let preview_kinds = manifold_core::flatten::flatten_groups(splice_def)
                .map(|flat| crate::node_graph::PreviewEncoding::propagate(&flat))
                .unwrap_or_default();
            // Build the unified resolved-binding list: static prefix
            // first (view.bindings → ResolvedBinding::from_static),
            // then user tail (per-instance UserParamBinding →
            // ResolvedBinding::from_user). Each binding carries its
            // own `source_index` into `PresetInstance.param_values` —
            // resolved by matching `BindingDef::id` against the
            // outer-card param list (NOT the binding's own enumerate
            // position) so a single outer slider can fan out to
            // multiple inner-node params and the second/third binding
            // still reads the right slot. Mirrors the generator-side
            // shape — see
            // `JsonGraphGenerator::from_def`'s `outer_param_index`.
            let outer_param_index: ahash::AHashMap<&str, usize> = view
                .canonical_def
                .preset_metadata
                .as_ref()
                .map(|m| {
                    m.params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| (p.id.as_str(), i))
                        .collect()
                })
                .unwrap_or_default();
            let n_static_slots = view
                .canonical_def
                .preset_metadata
                .as_ref()
                .map(|m| m.params.len())
                .unwrap_or(view.bindings.len());
            // User-added bindings now live in the per-instance graph's
            // `preset_metadata` (the single binding-storage list);
            // `user_param_bindings()` synthesizes the runtime view (routing
            // from the binding + range from its reshape note).
            let user_bindings = fx.user_param_bindings();
            let mut bindings: Vec<ResolvedBinding> =
                Vec::with_capacity(view.bindings.len() + user_bindings.len());
            // Retain the static specs + source_index so the static prefix
            // can be rebuilt in place when a per-instance reshape note
            // changes (see the `param_mappings_version` watch in `run`).
            let mut static_specs: Vec<(ParamBinding, usize)> =
                Vec::with_capacity(view.bindings.len());
            for b in view.bindings.iter() {
                let source_index = outer_param_index
                    .get(b.id.as_ref())
                    .copied()
                    .unwrap_or(0);
                // Per-instance reshape note for this stock param, if the
                // user has reshaped it. `None` is byte-identical to before.
                let note = fx.param_mapping(b.id.as_ref());
                match ResolvedBinding::from_static(b, &node_map, source_index, note) {
                    Some(rb) => {
                        bindings.push(rb);
                        static_specs.push((b.clone(), source_index));
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
            let n_static = bindings.len();
            for (user_slot, core) in user_bindings.iter().enumerate() {
                let source_index = n_static_slots + user_slot;
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
                        c
                    });
                let resolve_core = retargeted.as_ref().unwrap_or(core);
                match ResolvedBinding::from_user(resolve_core, &graph, &node_map, source_index) {
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
            let bound = BoundGraph::new(bindings, &mut graph);
            // The user tail lives in the graph now, so its rebuild signal
            // is the graph version (a binding add/remove/reshape bumps it).
            let user_bindings_version = fx.graph_version;
            let param_mappings_version = fx.param_mappings_version;
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
                n_static,
                n_static_slots,
                user_bindings_version,
                generator_input_node: generator_input_id,
                static_specs,
                param_mappings_version,
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
            built_generation: crate::preset_loader::catalog_generation(),
            state_store: StateStore::new(),
            errors,
            preview_encoding: crate::node_graph::PreviewEncoding::default(),
        })
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
                slot.bound
                    .apply_inner_overrides(&mut self.graph, &slot.node_map, fx.graph.as_ref());
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
                slot.bound.bindings.truncate(slot.n_static);
                for (user_slot, core) in fx.user_param_bindings().iter().enumerate() {
                    let source_index = slot.n_static_slots + user_slot;
                    match ResolvedBinding::from_user(
                        core,
                        &self.graph,
                        &slot.node_map,
                        source_index,
                    ) {
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
                apply_binding_defaults(&slot.bound.bindings[slot.n_static..], &mut self.graph, None);
                slot.user_bindings_version = fx.graph_version;
                // Reset the user-tail cache so the first apply after
                // re-hydrate unconditionally writes — the previous
                // cache entries refer to a different binding list and
                // would skip-write on stale-prev compare.
                slot.bound.cache.clear_tail(slot.n_static);
            }
            // Re-build the static prefix if a per-instance reshape note
            // changed. A mapping-drawer edit bumps `param_mappings_version`
            // without touching topology, so rebuild the static bindings
            // from the retained specs + current notes. The user tail
            // ([n_static..]) is untouched (notes are stock-param-only in
            // this pass). Clearing the whole cache forces the new reshape
            // to re-apply next frame even though the raw slot value didn't
            // move — the cache keys on the raw value, which a reshape edit
            // leaves unchanged, so without this the edit would be skipped.
            if fx.param_mappings_version != slot.param_mappings_version {
                let mut new_static: Vec<ResolvedBinding> =
                    Vec::with_capacity(slot.static_specs.len());
                for (b, source_index) in &slot.static_specs {
                    if let Some(rb) = ResolvedBinding::from_static(
                        b,
                        &slot.node_map,
                        *source_index,
                        fx.param_mapping(b.id.as_ref()),
                    ) {
                        new_static.push(rb);
                    }
                }
                // Resolution is deterministic against the stable handles,
                // so the rebuilt prefix length matches the original. Guard
                // against the impossible mismatch rather than corrupt the
                // user-tail offsets.
                if new_static.len() == slot.n_static {
                    slot.bound.bindings.splice(0..slot.n_static, new_static);
                    slot.bound.cache.clear();
                }
                slot.param_mappings_version = fx.param_mappings_version;
            }
            slot.bound.apply(&mut self.graph, &fx.param_values);
            // If the preset includes a `system.generator_input` node,
            // push every frame-context scalar (time / beat / aspect /
            // output dims) into its params. The standard port-shadows-
            // param machinery propagates these to inner primitives via
            // scalar wires — same surface generators have, no per-effect
            // Rust code. `trigger_count` / `anim_progress` are clip-side
            // concepts that don't reach the effect chain — they stay at
            // the generator_input primitive's default (0.0).
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
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("ChainGraph backend is MetalBackend");
        let ok = metal.replace_texture_2d(self.source_slot, input_texture.clone());
        debug_assert!(ok, "source slot pre-bound at build time");

        let frame_time = FrameTime {
            beats: manifold_core::Beats(ctx.beat),
            seconds: manifold_core::Seconds(ctx.time),
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

    /// Aim the authoring-time output preview at `node_id` within effect
    /// `effect_id`, or clear it. Resolves the editor's stable [`NodeId`] to
    /// the runtime node via the owning effect's `node_map`. A `None` node id,
    /// or an `effect_id` this chain doesn't hold, clears capture — so a chain
    /// that isn't the watched one contributes no stale preview. Call before
    /// [`Self::run`]; the preserved texture is then read via
    /// [`Self::preview_texture`].
    pub fn set_preview_target(&mut self, effect_id: &EffectId, node_id: Option<&NodeId>) {
        use crate::node_graph::PreviewEncoding;
        // `(target instance, encoding)`, resolved against the owning slot.
        let resolved: Option<(NodeInstanceId, PreviewEncoding)> = node_id.and_then(|nid| {
            self.effect_nodes
                .iter()
                .find(|slot| &slot.effect_id == effect_id)
                .and_then(|slot| {
                    // Direct node hit: prefer the propagated kind (so a blur of
                    // a field reads as a field); fall back to single-node derive.
                    if let Some((_, instance)) =
                        slot.node_map.iter().find(|(mapped, _)| mapped == nid)
                    {
                        let enc = slot
                            .preview_kinds
                            .get(nid)
                            .copied()
                            .unwrap_or_else(|| self.encoding_for_instance(*instance, None));
                        return Some((*instance, enc));
                    }
                    // A selected group container isn't in `node_map` (it
                    // flattened away); resolve it to its primary texture-output
                    // producer. The group's output port name is the strongest
                    // signal (`forceField`); else the producer's propagated kind.
                    slot.group_preview_map
                        .iter()
                        .find(|(group, _, _)| group == nid)
                        .and_then(|(_, producer, port)| {
                            slot.node_map
                                .iter()
                                .find(|(mapped, _)| mapped == producer)
                                .map(|(_, instance)| {
                                    let enc = PreviewEncoding::from_port_name(port)
                                        .or_else(|| slot.preview_kinds.get(producer).copied())
                                        .unwrap_or(PreviewEncoding::Color);
                                    (*instance, enc)
                                })
                        })
                })
        });
        let (target, encoding) = match resolved {
            Some((instance, encoding)) => (Some(instance), encoding),
            None => (None, PreviewEncoding::Color),
        };
        self.preview_encoding = encoding;
        self.executor.set_preview_target(target);
    }

    /// Single-node fallback when no propagated kind is on hand: derive from the
    /// runtime node's `type_id` and first output port off the live graph.
    fn encoding_for_instance(
        &self,
        inst: NodeInstanceId,
        port_override: Option<&str>,
    ) -> crate::node_graph::PreviewEncoding {
        let Some(n) = self.graph.get_node(inst) else {
            return crate::node_graph::PreviewEncoding::Color;
        };
        let port = port_override
            .or_else(|| n.node.outputs().first().map(|p| p.name))
            .unwrap_or("out");
        crate::node_graph::PreviewEncoding::derive(n.node.type_id().as_str(), port)
    }

    /// How the previewed node's output should be rendered (flow wheel / lift /
    /// raw). `Color` when this chain holds no watched node.
    pub fn preview_encoding(&self) -> crate::node_graph::PreviewEncoding {
        self.preview_encoding
    }

    /// Live scalar I/O of the previewed node — for the editor's value inspector
    /// when the watched node has no image output.
    pub fn preview_scalar_io(&self) -> crate::node_graph::PreviewScalarIo {
        (
            self.executor.preview_scalar_inputs().to_vec(),
            self.executor.preview_scalar_outputs().to_vec(),
        )
    }

    /// Clear any preview capture on this chain. Called each frame for chains
    /// that don't hold the watched effect so a stale target doesn't keep a
    /// texture pinned.
    pub fn clear_preview_target(&mut self) {
        self.executor.set_preview_target(None);
        self.preview_encoding = crate::node_graph::PreviewEncoding::Color;
    }

    /// The preview target's captured output texture from the most recent
    /// [`Self::run`], if this chain holds the watched node and it produced a
    /// texture. `None` otherwise (no target, target pruned, or non-texture
    /// output). See [`Executor::preview_resource`](crate::node_graph::Executor::preview_resource).
    pub fn preview_texture(&self) -> Option<&GpuTexture> {
        let res = self.executor.preview_resource()?;
        let slot = self.executor.backend().slot_for(res)?;
        self.executor.backend().texture_2d(slot)
    }

    /// Enable one-shot "dump every output" mode iff this chain holds
    /// `dump_effect`; otherwise disable it. Call each frame with the requested
    /// effect (or `None`) so only the watched effect's chain pays the cost.
    pub fn set_dump(&mut self, dump_effect: Option<&EffectId>) {
        let on =
            dump_effect.is_some_and(|eid| self.effect_nodes.iter().any(|s| &s.effect_id == eid));
        self.executor.set_dump_all(on);
    }

    /// After a `run` with dump mode on, every captured Texture2D output that
    /// belongs to effect `effect_id`, as `(node_id, port, type_id, texture)`.
    /// Filtered to the watched effect's nodes via its `node_map` so the dump is
    /// one effect's pipeline, not the whole spliced chain.
    pub fn dump_textures(
        &self,
        effect_id: &EffectId,
    ) -> Vec<(String, String, String, &GpuTexture)> {
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &(node, port, res) in self.executor.dump_resources() {
            // Only this effect's nodes (reverse-map runtime id → stable NodeId).
            let Some((node_id, _)) = slot.node_map.iter().find(|(_, niid)| *niid == node) else {
                continue;
            };
            let Some(tex) = self
                .executor
                .backend()
                .slot_for(res)
                .and_then(|s| self.executor.backend().texture_2d(s))
            else {
                continue;
            };
            let type_id = self
                .graph
                .get_node(node)
                .map(|inst| inst.node.type_id().as_str().to_string())
                .unwrap_or_default();
            out.push((node_id.to_string(), port.to_string(), type_id, tex));
        }
        out
    }

    /// Captured `Array` (storage-buffer) outputs of effect `effect_id` after a
    /// dump `run`, with their channel layout — the array counterpart of
    /// [`Self::dump_textures`].
    pub fn dump_arrays(&self, effect_id: &EffectId) -> Vec<crate::compositor::ArrayDump<'_>> {
        use crate::node_graph::ports::{ChannelElementType, PortType, std430_layout};
        let kind = |t: ChannelElementType| match t {
            ChannelElementType::F32 => "f32",
            ChannelElementType::I32 => "i32",
            ChannelElementType::U32 => "u32",
            ChannelElementType::Vec2F => "vec2f",
            ChannelElementType::Vec3F => "vec3f",
            ChannelElementType::Vec4F => "vec4f",
        };
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &(node, port, res) in self.executor.dump_array_resources() {
            let Some((node_id, _)) = slot.node_map.iter().find(|(_, niid)| *niid == node) else {
                continue;
            };
            let Some(PortType::Array(at)) = self.plan.resource_type(res) else {
                continue;
            };
            let Some(buffer) = self
                .executor
                .backend()
                .slot_for(res)
                .and_then(|s| self.executor.backend().array_buffer(s))
            else {
                continue;
            };
            let (offsets, _, _) = std430_layout(at.specs);
            let fields = at
                .specs
                .iter()
                .zip(offsets)
                .map(|(spec, off)| {
                    let name = spec
                        .name
                        .debug_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("ch@{off}"));
                    (name, kind(spec.ty), off)
                })
                .collect();
            let type_id = self
                .graph
                .get_node(node)
                .map(|inst| inst.node.type_id().as_str().to_string())
                .unwrap_or_default();
            out.push(crate::compositor::ArrayDump {
                name: node_id.to_string(),
                port: port.to_string(),
                type_id,
                buffer,
                item_size: at.item_size,
                fields,
            });
        }
        out
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
/// active effects and drops any whose `is_skipped_for(view.skip_mode, …, fx)`
/// returns `true`, so flipping that predicate (typically by dragging
/// `amount` off / onto 0) changes which effects appear in the graph.
/// We hash the predicate's current result per effect so the rebuild
/// fires when the user drags `amount` away from 0 — without it the
/// freshly-added effect would never enter the graph until the user
/// toggled `enabled` (which IS in the hash) to force a rebuild.
fn compute_topology_hash(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    width: u32,
    height: u32,
    preview_effect: Option<&EffectId>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    for fx in effects {
        fx.id.as_str().hash(&mut h);
        fx.effect_type().as_str().hash(&mut h);
        fx.enabled.hash(&mut h);
        // Watched (open-in-editor) target: folded into the rebuild key so
        // opening or closing the editor rebuilds exactly the chain holding this
        // effect, flipping it fused ⇄ unfused (the gate at `should_render_fused`
        // only re-runs on rebuild). Membership-local — `preview_effect` that
        // isn't among these effects hashes `false` for every one, so unrelated
        // chains are untouched and don't churn when the editor opens elsewhere.
        (preview_effect == Some(&fx.id)).hash(&mut h);
        match fx.group_id.as_ref() {
            Some(g) => g.as_str().hash(&mut h),
            None => "".hash(&mut h),
        }
        // Per-card graph divergence — but keyed on the *structure* version,
        // not the snapshot `graph_version`. Only a topology change (node/wire
        // add or remove, full revert) bumps this and forces a rebuild that
        // wipes primitive state. A value-only param edit or a node move bumps
        // only `graph_version` (for the UI snapshot) and is applied in place by
        // `run`'s `apply_inner_param_overrides`, so feedback/sim state survives.
        fx.graph_structure_version.hash(&mut h);
        // Skip-on-zero predicate state — see the doc-comment above.
        // Effects without a `LoadedPresetView` are ignored here
        // (legacy fallback); `try_build` will short-circuit anyway.
        if let Some(view) = loaded_preset_view_by_id(fx.effect_type()) {
            is_skipped_for(view.skip_mode, &view.type_id, fx).hash(&mut h);
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
/// Persistent resources — those identified by
/// [`ExecutionPlan::persistent_resources`] as carrying state across
/// frame boundaries — also get dedicated, non-recyclable slots. The
/// per-frame producer write and the per-frame consumer read must
/// land on the SAME physical texture (that's the feedback loop), but
/// that physical texture must not be shared with any other resource
/// whose lifetime overlaps the persistent's full-frame window —
/// otherwise an intermediate write through a recycled slot would
/// clobber the carry-over before the consumer reads it next frame.
/// Pre-allocating dedicated slots is the simplest correctness fix:
/// the slot never enters the free pool, so no other resource can be
/// assigned to it later in the simulator's walk.
///
/// The simulator's slot ids are dense `0..K`. The caller maps them to
/// real backend slots 1:1 via `allocate_slot`.
fn assign_texture2d_slots(plan: &ExecutionPlan, source_resource: ResourceId) -> SlotAssignment {
    let mut resource_to_slot: AHashMap<ResourceId, Slot> = AHashMap::default();
    let source_slot = Slot(0);
    resource_to_slot.insert(source_resource, source_slot);
    let mut next_slot: u32 = 1;

    // Pre-allocate dedicated slots for every persistent Texture2D
    // resource BEFORE the topological walk. These slots stay out of
    // the free pool for the rest of the simulation, guaranteeing the
    // feedback loop's producer/consumer share the carry-over texture
    // without any intermediate write aliasing it.
    let persistent_set: std::collections::HashSet<ResourceId> = plan
        .persistent_resources()
        .iter()
        .filter(|&&res_id| {
            plan.resource_type(res_id)
                .map(|ty| ty.is_texture_2d())
                .unwrap_or(false)
        })
        .copied()
        .collect();
    for &res_id in &persistent_set {
        let slot = Slot(next_slot);
        next_slot += 1;
        resource_to_slot.insert(res_id, slot);
    }

    let mut free_pool: Vec<Slot> = Vec::new();

    for step in plan.steps() {
        // Acquire output slots — pop from free pool or grow.
        for &(_, res_id) in &step.outputs {
            if res_id == source_resource {
                continue;
            }
            if persistent_set.contains(&res_id) {
                // Dedicated slot pre-allocated above. The producer's
                // write goes through `resource_to_slot[res_id]` at
                // runtime; nothing to do in the simulator.
                continue;
            }
            if !plan
                .resource_type(res_id)
                .map(|ty| ty.is_texture_2d())
                .unwrap_or(false)
            {
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
            if persistent_set.contains(&res_id) {
                // Persistent slots never enter the free pool.
                // (Compile-time invariant: persistent resources never
                // appear in any step's `free_after` — this guard is
                // defensive.)
                continue;
            }
            if !plan
                .resource_type(res_id)
                .map(|ty| ty.is_texture_2d())
                .unwrap_or(false)
            {
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

// (`pre_allocate_array_buffers_effect` was the stop-gap shim added in
// commit 3500e7a7 to fix the Blob Track drift bug. Both callers now
// route through `node_graph::graph_loader::pre_allocate_resources`
// which adds Texture3D + canvas-sized + post-allocation audit.)

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
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{EffectGroup, PresetInstance};
    use manifold_core::id::EffectGroupId;
    

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    #[test]
    fn non_contiguous_group_builds_multi_segment_mix() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        // Chain: Invert(g1) → ChromaticAberration → Invert(g1)
        // Effects on either side belong to g1; the middle effect doesn't.
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
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
            ChainGraph::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None);

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
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let mut e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        e2.group_id = Some(g1_id.clone());
        let e3 = make_default(PresetTypeId::INVERT_COLORS);

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            ChainGraph::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None);

        let cg = result.expect("ChainGraph should build for contiguous group");
        assert_eq!(cg.group_mix_nodes.len(), 1);
    }

    #[test]
    fn three_segment_group_builds_three_mix_sub_graphs() {
        // Chain: Invert(g1) → Chroma → Invert(g1) → Chroma → Invert(g1)
        // Group g1 has three non-contiguous segments.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());
        let e4 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e5 = make_default(PresetTypeId::INVERT_COLORS);
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
            None,
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
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    /// SoftFocus is the canonical reproducer: its outer `radius`
    /// binding default is `6.0`, but the underlying `Blur` primitive's
    /// `ParamDef::default` is `4.0`. Without the seed pass, the inner
    /// node starts at `4.0` and the user has to touch the slider for
    /// the cache compare to diverge and the binding to actually write.
    #[test]
    fn soft_focus_inner_blur_starts_at_binding_default_not_primitive_default() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::SOFT_FOCUS_GRAPH);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
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
            .cloned()
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
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    #[test]
    fn hash_changes_when_effect_becomes_the_watched_preview_target() {
        // Opening the graph editor on an effect must rebuild the chain holding
        // it so it flips fused → unfused (per-node preview + live edits). The
        // gate at `should_render_fused` only re-runs on rebuild, so the watched
        // flag has to move the topology hash. Membership-local: a `preview_effect`
        // that isn't in the chain leaves the hash unchanged (no churn elsewhere).
        let fx = make_default(PresetTypeId::COLOR_GRADE);
        let other = make_default(PresetTypeId::VORONOI_PRISM);

        let unwatched = compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, None);
        let watched =
            compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, Some(&fx.id));
        assert_ne!(
            unwatched, watched,
            "topology hash must change when an effect becomes the watched target \
             — otherwise opening its editor never rebuilds it unfused.",
        );

        // A watch on an effect NOT in this chain must not perturb the hash.
        let watch_elsewhere =
            compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, Some(&other.id));
        assert_eq!(
            unwatched, watch_elsewhere,
            "watching an effect absent from this chain must leave its hash \
             unchanged — unrelated chains must not churn when the editor opens.",
        );
    }

    #[test]
    fn hash_changes_when_skip_predicate_flips() {
        // Dragging an effect's `amount` slider across 0 must change
        // the topology hash so the chain rebuilds — without that, the
        // effect can't transition between "in graph" and "skipped"
        // states without a separate enabled toggle.
        //
        // Set up the test scenario explicitly: amount=0 first, then
        // amount=0.5. The §9.1.5 audit moved most effects' default
        // amount off zero, so we can't rely on the default for this
        // fixture.
        let mut fx = make_default(PresetTypeId::VORONOI_PRISM);
        fx.set_base_param(0, 0.0);

        let hash_at_zero = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        fx.set_base_param(0, 0.5);
        let hash_at_half = compute_topology_hash(&[fx], &[], 256, 256, None);

        assert_ne!(
            hash_at_zero, hash_at_half,
            "topology hash must change when an effect's SkipMode::OnZero \
             predicate flips — otherwise the chain doesn't rebuild and \
             the user has to toggle enabled to bring the effect into the \
             graph. See the doc-comment on `compute_topology_hash`."
        );
    }

    #[test]
    fn disabled_effects_are_excluded_from_active_set_and_change_hash() {
        // The user-facing invariant for the on/off toggle: setting
        // `enabled = false` MUST (a) flip the topology hash so the chain
        // rebuilds, and (b) exclude the effect from `active_effects` in
        // `try_build` so it stops rendering. Without these the toggle
        // appears to do nothing.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::MIRROR); // `amount` default = 1.0, so present in chain by default.
        assert!(fx.enabled, "PresetInstance::new defaults enabled = true");

        let hash_on = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_on = ChainGraph::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None)
            .expect("Mirror chain builds at enabled = true");
        assert_eq!(
            cg_on.effect_nodes.len(),
            1,
            "Mirror should contribute one effect slot when enabled",
        );

        fx.enabled = false;
        let hash_off = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        assert_ne!(
            hash_on, hash_off,
            "Toggling `enabled` MUST change the topology hash — otherwise the \
             chain caches the previous topology and the toggle appears dead.",
        );

        // With this as the only effect, the chain should refuse to build
        // (no active effects → None) — equivalent to "the chain becomes empty".
        let cg_off = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None);
        assert!(
            cg_off.is_none(),
            "Disabled effect must be filtered out of active_effects — got a chain with effects when it should be empty",
        );
    }

    #[test]
    fn value_edit_keeps_hash_but_structure_edit_changes_it() {
        // The core of the "don't reset state on every edit" fix: a value- or
        // position-only graph edit bumps `graph_version` (for the UI snapshot)
        // but NOT `graph_structure_version`, so the topology hash is unchanged
        // and the chain is NOT rebuilt (state preserved). Only a structural
        // edit moves the hash.
        let mut fx = make_default(PresetTypeId::MIRROR);
        let base = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        // Value / position edit: snapshot version moves, structure doesn't.
        fx.graph_version = fx.graph_version.wrapping_add(1);
        assert_eq!(
            base,
            compute_topology_hash(&[fx.clone()], &[], 256, 256, None),
            "a value/position edit must NOT change the topology hash (no rebuild, \
             state preserved)",
        );

        // Structural edit: structure version moves → hash changes → rebuild.
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        assert_ne!(
            base,
            compute_topology_hash(&[fx], &[], 256, 256, None),
            "a structural edit MUST change the topology hash so the chain rebuilds",
        );
    }

    #[test]
    fn stateful_effects_never_skip() {
        // Stateful effects must keep their workers alive across an
        // `amount → 0 → up` drag so their accumulated state (Feedback
        // prev-frame texture, Bloom mip pyramid, Watercolor ping-pong,
        // DNN worker spool, etc.) survives the bypass moment.
        // Tagging them `SkipMode::Never` is how we guarantee that.
        // Bloom is intentionally absent: its decomposed graph
        // (threshold → downsample → blur → mix) is stateless, so it has
        // no per-instance state to preserve and can stay SkipMode::OnZero.
        for ty in [
            PresetTypeId::STYLIZED_FEEDBACK,
            PresetTypeId::WATERCOLOR,
            PresetTypeId::DEPTH_OF_FIELD,
            PresetTypeId::WIREFRAME_DEPTH,
            PresetTypeId::BLOB_TRACKING,
            PresetTypeId::AUTO_GAIN,
        ] {
            let view = loaded_preset_view_by_id(&ty).unwrap_or_else(|| {
                panic!("{:?}: missing LoadedPresetView", ty);
            });
            assert!(
                matches!(view.skip_mode, crate::node_graph::SkipMode::Never),
                "{:?}: stateful effects must be SkipMode::Never so their \
                 per-instance state survives an amount → 0 → up slider drag",
                ty,
            );
        }
    }
}

#[cfg(test)]
mod user_binding_tests {
    //! Regression: a user-exposed inner-graph parameter must actually
    //! propagate its outer slot value to the inner node every frame.
    //!
    //! Pre-unification: the chain's per-frame apply called
    //! `apply_param_bindings(static, &[], …)`, so exposing a param via
    //! the graph editor produced a visible effect-card slider that
    //! silently wrote into a discarded list. The user-visible symptom:
    //! setting `Transform.rotation = 0.48` directly in the graph
    //! editor rotated the image, but exposing the same param on the
    //! Mirror card and dragging its slider to 0.48 did nothing.
    //!
    //! After the bindings unification (Phase 1) the runtime walks a
    //! single `slot.bindings: Vec<ResolvedBinding>` — the `&[]` bug
    //! class is structurally unrepresentable.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{
        PresetInstance, ParamMapping, ParamSlot, UserParamBinding, ParamConvert,
    };


    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    /// A bare scale-only reshape note for `param_id` (no invert/curve, so
    /// the normalize step is skipped and min/max are irrelevant). This is
    /// the smallest non-identity note — `inner = slot * scale`.
    fn scale_note(param_id: &str, scale: f32) -> ParamMapping {
        ParamMapping {
            param_id: param_id.to_string(),
            label: None,
            min: 0.0,
            max: 1.0,
            invert: false,
            curve: Default::default(),
            scale,
            offset: 0.0,
        }
    }

    fn affine_scale(cg: &ChainGraph, slot: &EffectSlot) -> ParamValue {
        let (_, affine_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        cg.graph
            .get_node(*affine_id)
            .and_then(|n| n.params.get("scale").cloned())
            .expect("affine_transform exposes a `scale` param")
    }

    /// Core model proof: a per-instance reshape NOTE on a STOCK param
    /// (`zoom` → `affine.scale`, recipe scale = identity) reshapes what
    /// the inner node sees, while the param's VALUE SLOT stays
    /// byte-identical — the load-bearing invariant for the live rig
    /// (Ableton / drivers / OSC / envelopes write that slot, untouched).
    #[test]
    fn stock_param_note_reshapes_inner_node_without_touching_the_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        // Mirror the per-frame apply `run()` performs: push the live
        // `param_values` through the slot's bindings into the graph.
        fn apply(cg: &mut ChainGraph, values: &[ParamSlot]) {
            let slot = &mut cg.effect_nodes[0];
            slot.bound.apply(&mut cg.graph, values);
        }

        // Control: same effect, zoom = 0.3, NO note → inner sees 0.3.
        let mut control = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        control.param_values[1] = ParamSlot::exposed(0.3); // zoom is static slot 1
        let mut cg0 =
            ChainGraph::try_build(std::slice::from_ref(&control), &[], &primitives, &device, None, 256, 256, None)
                .expect("control chain builds");
        apply(&mut cg0, &control.param_values);
        let slot0 = &cg0.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg0, slot0),
            ParamValue::Float(0.3),
            "without a note, the stock zoom slot value passes straight through",
        );

        // With a ×2 note on `zoom`: inner sees 0.6, slot still reads 0.3.
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        fx.param_values[1] = ParamSlot::exposed(0.3);
        fx.upsert_param_mapping(scale_note("zoom", 2.0));
        let mut cg =
            ChainGraph::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None)
                .expect("noted chain builds");
        apply(&mut cg, &fx.param_values);
        let slot = &cg.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg, slot),
            ParamValue::Float(0.6),
            "a ×2 reshape note must scale what the inner node sees (0.3 → 0.6)",
        );
        // The invariant: the value slot the modulation surface writes is
        // byte-identical with and without the note.
        assert_eq!(
            fx.param_values[1].value, 0.3,
            "the reshape note must NEVER rewrite the value slot — that slot \
             is what Ableton / drivers / OSC / envelopes address every frame",
        );
    }

    fn stylized_with_translate_exposed(translate_value: f32) -> PresetInstance {
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        // StylizedFeedback's graph registers an affine_transform under
        // the handle `"affine"`. Its static card exposes gain / scale /
        // rotation, but NOT `translate_x` — so a user-tail binding to
        // `affine.translate_x` is the sole writer of that inner param
        // (the clean regression vehicle the deleted Mirror.rotation
        // test used to be).
        fx.append_user_binding(UserParamBinding {
            id: "user.affine.translate_x.1".to_string(),
            label: "Translate X".to_string(),
            node_id: NodeId::new("affine"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: -1.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        });
        // Drag the user-tail slider to `translate_value`. With static
        // count = 3 (amount, zoom, rotate) the user binding's slot lives
        // at index 3.
        let slot_index = 3;
        assert_eq!(
            fx.param_values.len(),
            4,
            "StylizedFeedback with 3 static + 1 user-tail = 4 param slots",
        );
        fx.param_values[slot_index] = ParamSlot::exposed(translate_value);
        fx
    }

    /// Build-time hydrate: the chain's unified
    /// `EffectSlot.bindings` must include one entry per
    /// `fx.user_param_bindings` after the static prefix, each resolved
    /// to the correct inner node + param.
    #[test]
    fn build_time_hydrate_resolves_user_binding_to_inner_node() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = stylized_with_translate_exposed(0.48);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
            .expect("StylizedFeedback chain with one user binding builds");

        let slot = cg
            .effect_nodes
            .first()
            .expect("StylizedFeedback contributes one effect slot");
        assert_eq!(
            slot.bound.bindings.len(),
            slot.n_static + 1,
            "user-tail binding for affine.translate_x must hydrate at build time",
        );
        let user_rb = &slot.bound.bindings[slot.n_static];
        assert_eq!(user_rb.source, crate::node_graph::BindingSource::User);
        match &user_rb.target {
            crate::node_graph::ResolvedTarget::Node { param, .. } => {
                assert_eq!(*param, "translate_x");
            }
            _ => panic!("user binding must resolve to a Node target"),
        }
    }

    /// Per-frame apply: after build, calling `apply_bindings` with
    /// the chain's stored unified binding list must write the
    /// user-tail param value to the inner Transform node.
    #[test]
    fn exposed_slider_value_reaches_inner_node() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = stylized_with_translate_exposed(0.48);

        let mut cg = ChainGraph::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
        )
        .expect("StylizedFeedback chain with one user binding builds");

        // Mirror the per-frame apply that `run()` would execute:
        // walk the slot's unified bindings against fx.param_values.
        let slot = &mut cg.effect_nodes[0];
        slot.bound.apply(&mut cg.graph, &fx.param_values);

        // Inspect the inner affine node's `translate_x` param — it
        // must reflect the user-tail slot's value, not its primitive
        // default of 0.0.
        let (_, xform_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        let translate_x = cg
            .graph
            .get_node(*xform_id)
            .and_then(|n| n.params.get("translate_x").cloned())
            .expect("affine_transform exposes a `translate_x` param");

        assert_eq!(
            translate_x,
            ParamValue::Float(0.48),
            "exposed user-binding slider must propagate to the inner \
             affine.translate_x param. If this is `Float(0.0)`, the \
             per-frame apply walked the wrong slice — the regression \
             that motivated this fix.",
        );
    }

    /// Symmetric default-seed regression for user bindings — mirror
    /// of `binding_seed_tests::soft_focus_inner_blur_starts_at_binding_default_not_primitive_default`
    /// for the user tier.
    ///
    /// Builds a StylizedFeedback chain whose user-exposed
    /// `affine.translate_x` binding declares `default_value = 0.42`,
    /// and asserts that the inner affine node's `translate_x` param
    /// starts at `0.42` (the binding default) rather than `0.0` (the
    /// affine_transform primitive's `ParamDef::default`). Catches the
    /// latent "user binding default not seeded" bug: without the
    /// unified `apply_binding_defaults` walk covering the user tail,
    /// exposed sliders would have to be "touched" to push their
    /// declared default through.
    #[test]
    fn user_binding_with_nonzero_default_seeds_inner_at_build_time() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        fx.append_user_binding(UserParamBinding {
            id: "user.affine.translate_x.1".to_string(),
            label: "Translate X".to_string(),
            node_id: NodeId::new("affine"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: -1.0,
            max: 1.0,
            default_value: 0.42,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        });
        // Leave the outer slot at its declared default so the test
        // depends on the seed pass, not on the apply-with-divergent-
        // value path.
        let slot_index = 3;
        assert_eq!(fx.param_values.len(), 4);
        fx.param_values[slot_index] = ParamSlot::exposed(0.42);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
            .expect("StylizedFeedback chain with one user binding builds");
        let slot = cg
            .effect_nodes
            .first()
            .expect("StylizedFeedback contributes one effect slot");
        let (_, xform_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        let translate_x = cg
            .graph
            .get_node(*xform_id)
            .and_then(|n| n.params.get("translate_x").cloned())
            .expect("affine_transform exposes a `translate_x` param");
        assert_eq!(
            translate_x,
            ParamValue::Float(0.42),
            "user-binding default seed must plant 0.42 into affine.translate_x \
             at build time. If this is Float(0.0), the unified \
             apply_binding_defaults walk regressed and exposed sliders \
             will need to be 'touched' before they take their declared default.",
        );
    }
}

#[cfg(test)]
mod persistent_slot_tests {
    //! Regression: a feedback chain like
    //! `source → feedback → affine → gain → vignette → mix`
    //! where `mix.out` wires back to `feedback.in` (closing the per-
    //! frame loop) must NOT have `feedback.in`'s resource (which is
    //! `mix.out`) and `feedback.out`'s resource share a physical
    //! Texture2D slot.
    //!
    //! Without dedicated slots for persistent resources, the simulator's
    //! free-pool ping-pong was assigning the same slot to both:
    //! `feedback.out` got Slot(N) at step 0, was freed at step 2 when
    //! affine read it, and Slot(N) was eventually pulled out of the
    //! pool again at mix's step for `mix.out` — making them aliases.
    //! At runtime that turns Feedback's copy(prev→out) followed by
    //! copy(in→prev) into a no-op: `in` and `out` point at the same
    //! MTLTexture, so the "capture" step reads the value the "emit"
    //! step just wrote, never picking up the producer's actual write.
    //! Symptom: feedback effects look like a pass-through with no
    //! accumulation.
    //!
    //! The fix lives in `assign_texture2d_slots`: every persistent
    //! resource pre-allocates its own slot that never enters the free
    //! pool. This test pins the contract by constructing the exact
    //! topology and asserting the two slots differ.
    use super::*;
    use crate::node_graph::primitives::{
        AffineTransform, Feedback, Gain, Mix, Vignette,
    };
    use crate::node_graph::{FinalOutput, Graph, Source, compile};

    #[test]
    fn feedback_in_and_out_get_distinct_slots_in_the_closed_loop() {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let fb = graph.add_node(Box::new(Feedback::new()));
        let aff = graph.add_node(Box::new(AffineTransform::new()));
        let gain = graph.add_node(Box::new(Gain::new()));
        let vig = graph.add_node(Box::new(Vignette::new()));
        let mix = graph.add_node(Box::new(Mix::new()));
        let out = graph.add_node(Box::new(FinalOutput::new()));

        graph.connect((src, "out"), (mix, "a")).unwrap();
        graph.connect((fb, "out"), (aff, "in")).unwrap();
        graph.connect((aff, "out"), (gain, "in")).unwrap();
        graph.connect((gain, "out"), (vig, "in")).unwrap();
        graph.connect((vig, "out"), (mix, "b")).unwrap();
        // The state-capture edge — allowed because Feedback declares
        // `breaks_dependency_cycle`. This is the wire that would have
        // collapsed feedback.out and mix.out onto the same physical
        // slot under the pre-fix simulator.
        graph.connect((mix, "out"), (fb, "in")).unwrap();
        graph.connect((mix, "out"), (out, "in")).unwrap();

        let plan = compile(&graph).expect("feedback chain compiles");

        let src_res = plan
            .steps()
            .iter()
            .find(|s| s.node == src)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("source produces an out resource");
        let assignment = assign_texture2d_slots(&plan, src_res);

        let mix_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == mix)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("mix produces an out resource");
        let fb_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == fb)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("feedback produces an out resource");

        let mix_slot = assignment
            .resource_to_slot
            .get(&mix_out_res)
            .copied()
            .expect("mix.out has a slot");
        let fb_slot = assignment
            .resource_to_slot
            .get(&fb_out_res)
            .copied()
            .expect("feedback.out has a slot");

        assert_ne!(
            mix_slot, fb_slot,
            "mix.out and feedback.out MUST live on distinct physical slots. \
             Sharing a slot means feedback.in (which points at mix.out) and \
             feedback.out alias the same MTLTexture at runtime, and the \
             primitive's capture step reads back what its emit step just \
             wrote — feedback never accumulates state across frames. \
             Pre-fix, the simulator's free-pool ping-pong would assign \
             Slot(1) to both. The persistent-resource pre-allocation in \
             `assign_texture2d_slots` is what keeps them apart.",
        );

        // Sanity: the persistent resource's slot must be in the slot
        // assignment (mix.out is what feedback.in reads).
        let plan_persistent: std::collections::HashSet<_> =
            plan.persistent_resources().iter().copied().collect();
        assert!(
            plan_persistent.contains(&mix_out_res),
            "compile() must mark mix.out as a persistent resource — \
             without that, the slot simulator can't dedicate a slot for it"
        );
    }
}

#[cfg(test)]
mod generator_input_tests {
    //! Regression for the effect-side `system.generator_input` surface.
    //! Effects that include a `system.generator_input` node in their
    //! preset get per-frame scalars (time / beat / aspect / output
    //! dims) pushed to it by the chain runner, the same way generators
    //! do. The standard port-shadows-param machinery then propagates
    //! those scalars to inner primitives via wires — no per-effect
    //! Rust code, no hardcoded `apply_ctx_params_at` match list.
    //!
    //! These tests pin two contracts:
    //! 1. **Splice surface**: a preset that includes
    //!    `system.generator_input` causes [`SpliceResult::generator_input_id`]
    //!    to be `Some`, threaded onto [`EffectSlot::generator_input_node`].
    //! 2. **Per-frame push**: [`ChainGraph::run`] writes the
    //!    [`PresetContext`]'s `time` / `beat` / `aspect` / output dims
    //!    into the generator_input node's params via `set_param`.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    /// A divergent PresetInstance whose graph contains a
    /// `system.generator_input` node. Uses Invert as the host effect
    /// type so we get a known canonical to override; the divergent def
    /// is what actually drives splicing.
    fn invert_with_generator_input() -> PresetInstance {
        let custom_def: EffectGraphDef = serde_json::from_str(
            r#"{
                "version": 1,
                "name": "test",
                "nodes": [
                    { "id": 0, "typeId": "system.source" },
                    { "id": 1, "typeId": "system.generator_input", "handle": "input" },
                    { "id": 2, "typeId": "node.invert", "handle": "invert" },
                    { "id": 3, "typeId": "system.final_output" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                    { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
                ]
            }"#,
        )
        .expect("test fixture parses");

        let mut fx = make_default(PresetTypeId::INVERT_COLORS);
        // Mark the divergent path live so try_build picks it up. A divergent
        // def is a structural change, so bump the structure version too.
        fx.graph = Some(custom_def);
        fx.graph_version = fx.graph_version.wrapping_add(1);
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        fx
    }

    /// Build-time contract: a divergent def with a
    /// `system.generator_input` node populates the EffectSlot's
    /// `generator_input_node` field.
    #[test]
    fn splice_threads_generator_input_id_onto_effect_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
            .expect("chain builds with a divergent def including system.generator_input");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_some(),
            "EffectSlot.generator_input_node must populate when the def \
             includes a system.generator_input node — without this the \
             chain runner has nowhere to push frame-context scalars and \
             effects can't react to project time/beat."
        );
    }

    /// Build-time symmetry: presets without `system.generator_input`
    /// leave `EffectSlot.generator_input_node` as `None`. Most
    /// shipping effects today fall in this bucket — the field is
    /// opt-in.
    #[test]
    fn splice_leaves_generator_input_node_none_when_absent() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Canonical Invert preset has no system.generator_input.
        let fx = make_default(PresetTypeId::INVERT_COLORS);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
            .expect("Invert chain builds without divergent def");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_none(),
            "EffectSlot.generator_input_node should stay None when the \
             preset doesn't include a system.generator_input — opt-in surface."
        );
    }

    /// Per-frame contract: after `ChainGraph::run`, the generator_input
    /// node's `time` / `beat` / `aspect` / `output_width` /
    /// `output_height` params reflect the [`PresetContext`].
    /// Exercises the param-write half of the system; the
    /// scalar-wire-propagation half is covered by the
    /// `generator_input_params_drive_scalar_outputs` test in
    /// `boundary_nodes.rs`.
    #[test]
    fn run_pushes_frame_context_into_generator_input_params() {
        use crate::preset_context::{PresetContext, MAX_GEN_PARAMS};
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            ChainGraph::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        // A dummy input texture for `run` to install into the source slot.
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );

        let mut native_enc = device.create_encoder("generator-input-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        let ctx = PresetContext {
            time: 1.5,
            beat: 2.25,
            dt: 1.0 / 60.0,
            width: 1920,
            height: 1080,
            output_width: 3840,
            output_height: 2160,
            aspect: 1920.0 / 1080.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
            params: [0.0; MAX_GEN_PARAMS],
            param_count: 0,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let read = |name: &str| -> Option<f32> {
            node.params.get(name).and_then(|v| match v {
                ParamValue::Float(f) => Some(*f),
                _ => None,
            })
        };
        assert_eq!(read("time"), Some(1.5));
        assert_eq!(read("beat"), Some(2.25));
        // aspect derives from ctx.width / ctx.height (the render-resolution
        // dims, not the upscale-target output_* fields).
        assert!((read("aspect").unwrap() - (1920.0 / 1080.0)).abs() < 1e-5);
        assert_eq!(read("output_width"), Some(3840.0));
        assert_eq!(read("output_height"), Some(2160.0));
    }

    /// **The production main-path proof (design §12.3 step 5).** With the freeze
    /// toggle on (default), [`ChainGraph::try_build`] renders a canonical
    /// ColorGrade card through the FUSED node, not the 7 atoms: the built chain
    /// graph contains one `node.wgsl_compute` and none of the original
    /// `node.gain` / `node.mix` workers, and it runs one frame producing an
    /// output texture. This is what puts the optimised fused kernel on screen.
    #[test]
    fn colorgrade_chain_renders_via_fused_node() {
        use crate::preset_context::{PresetContext, MAX_GEN_PARAMS};
        use crate::gpu_encoder::GpuEncoder;

        // Honor the kill-switch: when MANIFOLD_FREEZE is off this path is
        // intentionally the unfused one, so the assertion wouldn't hold.
        if !crate::node_graph::freeze::install::freeze_enabled() {
            return;
        }

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::new("ColorGrade"));

        let mut cg = ChainGraph::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
        )
        .expect("ColorGrade chain builds");

        // Main-path proof: the fused kernel replaced the atom chain.
        let type_ids: Vec<&str> =
            cg.graph.nodes().map(|n| n.node.type_id().as_str()).collect();
        assert!(
            type_ids.contains(&"node.wgsl_compute"),
            "fused chain must contain the fused WGSL node; got {type_ids:?}"
        );
        assert!(
            !type_ids.contains(&"node.gain") && !type_ids.contains(&"node.mix"),
            "fused chain must NOT still contain unfused ColorGrade atoms; got {type_ids:?}"
        );

        // And it renders one frame, producing an output texture (the fused
        // kernel actually dispatched through the production chain).
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GRAPH_FORMAT,
            "cg-fused-input",
        );
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 256,
            height: 256,
            output_width: 256,
            output_height: 256,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
            params: [0.0; MAX_GEN_PARAMS],
            param_count: 0,
        };
        let mut native_enc = device.create_encoder("cg-fused-run");
        {
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);
            let out =
                cg.run(&mut gpu, &input.texture, std::slice::from_ref(&fx), &[], &ctx);
            assert!(out.is_some(), "fused ColorGrade chain produced an output texture");
        }
        native_enc.commit_and_wait_completed();
    }
}

#[cfg(test)]
mod chain_error_tests {
    //! The chain runner accumulates structured errors during build
    //! and per-frame run. Each entry carries the effect's identity
    //! so a future editor surface can attach it to the right card.
    //!
    //! Today the immediate user-visible benefit is the consistent
    //! `[chain-error]` terminal log; tomorrow these are the data
    //! the editor reads via [`ChainGraph::errors`]. The tests below
    //! pin one variant from the per-build path so the surface
    //! doesn't silently regress.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{PresetInstance, ParamConvert, UserParamBinding};

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::effect::create_default(&ty)
    }

    /// A user-exposed binding pointing at a handle the splice didn't
    /// register surfaces as a structured `UserBindingResolveFailed`
    /// entry on the chain's error log. Pre-change: this was a bare
    /// `eprintln!` with no programmatic surface — the editor couldn't
    /// highlight the broken slider.
    #[test]
    fn unresolved_user_binding_surfaces_as_structured_chain_error() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::INVERT_COLORS);
        // Reference a handle that the canonical Invert splice does
        // NOT register. Resolution fails at build time → records a
        // UserBindingResolveFailed error and the slider stays inert.
        fx.append_user_binding(UserParamBinding {
            id: "user.broken.1".to_string(),
            label: "Broken".to_string(),
            node_id: NodeId::new("does_not_exist"),
            legacy_node_handle: None,
            inner_param: "amount".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        });

        let cg = ChainGraph::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None)
            .expect("Invert chain still builds; the binding just fails to resolve");

        let errors = cg.errors();
        let matching = errors.iter().find(|e| {
            matches!(
                e,
                ChainError::UserBindingResolveFailed {
                    binding_id,
                    node_id,
                    rehydrate: false,
                    ..
                } if binding_id == "user.broken.1" && node_id == "does_not_exist"
            )
        });
        assert!(
            matching.is_some(),
            "expected a UserBindingResolveFailed entry naming the broken binding; \
             got {errors:?}",
        );
    }

    /// Sanity: a chain whose effects all resolve cleanly has an
    /// empty error log. Paired with the negative test so a
    /// regression that always-records or always-reads-empty
    /// surfaces visibly.
    #[test]
    fn clean_chain_has_no_errors() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::INVERT_COLORS);

        let cg = ChainGraph::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None)
            .expect("clean Invert chain builds");

        assert!(
            cg.errors().is_empty(),
            "clean chain must have no structured errors; got {:?}",
            cg.errors()
        );
    }
}
