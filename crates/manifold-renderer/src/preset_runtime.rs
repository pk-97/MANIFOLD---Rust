//! [`PresetRuntime`] — one cached [`Graph`] per `EffectChain`.
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
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    BindingSource, BoundGraph, EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID,
    FinalOutput, FrameTime, GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LoadError, LoadedPresetView,
    MetalBackend, NodeInstanceId, ParamBinding, ParamValue, PrimitiveRegistry, ResolvedBinding,
    ResolvedTarget,
    ResourceId, Slot, Source, SpliceResult, StateStore, apply_binding_defaults, compile,
    splice_def_into_chain,
};
use crate::node_graph::{is_skipped_for, loaded_preset_view_by_id};
use crate::preset_context::PresetContext;
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef};
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, Seconds};
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
// PresetRuntime — one Graph per EffectChain (linear, no wet/dry groups)
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
    /// Input/output model. `Transform` (effect chain) installs an upstream
    /// input texture into a dedicated source slot each frame and owns its
    /// output slot (the host reads `output_texture()`). `Generate` (generator)
    /// has no input — it renders *into* a host-provided target texture
    /// installed at the `final_output` source slot each frame.
    io: PresetIo,
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
    /// True when at least one segment of this chain was Pending (background
    /// compile in flight) at build time, so the chain spliced those cards
    /// per-card. The dispatcher rebuilds when the segment generation advances,
    /// picking up the fused winner (or the cached refusal) — the swap-in
    /// trigger. False once every segment resolved either way.
    pending_segments: bool,
    /// [`crate::node_graph::freeze::install::segment_generation`] observed at
    /// build time. Compared by the dispatcher only while
    /// [`Self::pending_segments`] is set.
    built_segment_generation: u64,
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

    // ---- Generator-only state (empty / None for effect-chain runtimes) ----
    /// Stable identity for the `GeneratorRegistry`. `Some` for generators
    /// (built via [`Self::from_def`] / [`Self::from_def_with_device`]); `None`
    /// for effect chains (which are addressed by `EffectId` per segment).
    type_id: Option<PresetTypeId>,
    /// Texture format threaded through to placeholder allocation on a generator
    /// `resize`. `None` for effect chains and the mock-backend test path.
    target_format: Option<GpuTextureFormat>,
    /// String-typed outer-card → inner-node bindings. Generators only — the
    /// shared float `apply` loop can't carry `String` params. Empty for chains.
    string_bindings: Vec<StringBindingResolution>,
}

/// Input/output model for a [`PresetRuntime`]. The one genuine difference
/// between an effect chain and a generator: an effect transforms an upstream
/// input texture; a generator produces from nothing and writes into a
/// host-provided target.
enum PresetIo {
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

/// One resolved String outer-card → inner-node binding (generators only). The
/// String binding path stays bespoke (the shared float `apply` loop is
/// float-only): source is keyed by name (lookup into the host's
/// `clip.string_params` map), no convert because `String → String` is a
/// pass-through.
struct StringBindingResolution {
    target_node: NodeInstanceId,
    target_param: String,
    /// Key into the host's `clip.string_params` map. The `presetMetadata`
    /// `stringBindings` `id` field — same identity as the matching
    /// `stringParams` entry's `id`.
    source_key: String,
    default: String,
    /// The def node's OWN value for the target param, captured from the
    /// (flattened) def at resolution time (BUG-182). Wins over `default`
    /// when seeding at construction, so a def-baked value — a file path set
    /// directly on the node, as the glb importer's mesh sources rely on —
    /// survives the build. `None` when the def leaves the param unset.
    def_value: Option<String>,
}

/// Read a def-baked String param value for string-binding seeding: the
/// flattened def's node matching `node_id`, its literal `param` value if the
/// def sets one (BUG-182 — the def node param wins over the binding's
/// declared default at construction). Non-String serialized values can't
/// occur for a String-typed param (the loader type-checks), but a mismatch
/// degrades to `None` (= seed from the binding default) rather than failing
/// the build.
fn def_string_param_value(
    flat_def: &manifold_core::effect_graph_def::EffectGraphDef,
    node_id: &manifold_core::NodeId,
    param: &str,
) -> Option<String> {
    let value = flat_def
        .nodes
        .iter()
        .find(|n| &n.node_id == node_id)?
        .params
        .get(param)?;
    match value {
        manifold_core::effect_graph_def::SerializedParamValue::String { value } => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// Errors produced when loading a generator preset (the generator
/// construction path of [`PresetRuntime`]).
#[derive(Debug)]
pub enum JsonGeneratorLoadError {
    /// JSON parsing failed.
    Json(serde_json::Error),
    /// The schema document failed to construct a Graph.
    Load(LoadError),
    /// The compiled graph had a static error (cycle, type mismatch, …).
    Compile(GraphError),
    /// The preset's JSON contains no `system.generator_input` node.
    MissingGeneratorInput,
    /// The preset's JSON contains no `system.final_output` node, or it
    /// isn't wired.
    MissingFinalOutput,
    /// BUG-125: the preset's JSON contains MORE THAN ONE `system.final_output`
    /// node. The tracked-output resolution (`graph.nodes().find(...)`) is a
    /// single, unordered lookup — a second `final_output` would be picked
    /// nondeterministically per process, and the per-frame canvas-target
    /// rebind (`replace_texture_2d`) would silently overwrite whichever one
    /// lost with the host canvas's format, up to a real GPU command-buffer
    /// fault. Rejected at load rather than silently picked.
    MultipleFinalOutputs { count: usize },
    /// A primitive declared an `Array<T>` output but
    /// `EffectNode::array_output_capacity` returned `None` for that port.
    UnsizedArrayOutput { node_type: String, port: String },
    /// Sibling of `UnsizedArrayOutput` for Texture3D.
    UnsizedTexture3DOutput { node_type: String, port: String },
    /// Post-allocation catch-all: an `Array<T>` resource in the compiled plan
    /// has no bound slot or no underlying buffer.
    UnboundArrayResource {
        producer_handle: Option<String>,
        producer_node_type: String,
        producer_port: String,
        cause: &'static str,
    },
}

impl std::fmt::Display for JsonGeneratorLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "JSON parse error: {e}"),
            Self::Load(e) => write!(f, "graph load error: {e}"),
            Self::Compile(e) => write!(f, "graph compile error: {e:?}"),
            Self::MissingGeneratorInput => write!(
                f,
                "preset has no `{GENERATOR_INPUT_TYPE_ID}` node — required for generator graphs"
            ),
            Self::MissingFinalOutput => write!(
                f,
                "preset has no `{FINAL_OUTPUT_TYPE_ID}` node, or it is not wired"
            ),
            Self::MultipleFinalOutputs { count } => write!(
                f,
                "preset has {count} `{FINAL_OUTPUT_TYPE_ID}` nodes — exactly one is \
                 required; the tracked-output resolution can't disambiguate more than \
                 one (see BUG-125). Wire extra outputs to a non-FinalOutput dead-end \
                 sink and inspect via `dump_textures_all` instead."
            ),
            Self::UnsizedArrayOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Array<T> output port `{port}` has no \
                 concrete size — `array_output_capacity` returned None. \
                 Add a `max_capacity` param, or override the method to derive \
                 size from a forward-dep input (not a state-capture port)."
            ),
            Self::UnsizedTexture3DOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Texture3D output port `{port}` has no \
                 concrete dims — `texture_3d_output_dims` returned None. \
                 Add `vol_res` / `vol_depth` params, or override the method to \
                 derive dims from a forward-dep input."
            ),
            Self::UnboundArrayResource {
                producer_handle,
                producer_node_type,
                producer_port,
                cause,
            } => {
                let handle_part = match producer_handle {
                    Some(h) => format!(" (handle `{h}`)"),
                    None => String::new(),
                };
                write!(
                    f,
                    "Array<T> output of `{producer_node_type}.{producer_port}`{handle_part} \
                     has no bound buffer after chain build: {cause}. \
                     This is the post-allocation audit catching a wire \
                     whose source resource was never pre-bound."
                )
            }
        }
    }
}

impl std::error::Error for JsonGeneratorLoadError {}

impl From<serde_json::Error> for JsonGeneratorLoadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<LoadError> for JsonGeneratorLoadError {
    fn from(e: LoadError) -> Self {
        Self::Load(e)
    }
}
impl From<GraphError> for JsonGeneratorLoadError {
    fn from(e: GraphError) -> Self {
        Self::Compile(e)
    }
}

/// Map a [`crate::node_graph::PreAllocationError`] into [`JsonGeneratorLoadError`].
fn generator_error_from_prealloc(
    e: crate::node_graph::PreAllocationError,
) -> JsonGeneratorLoadError {
    use crate::node_graph::PreAllocationError as P;
    match e {
        P::UnsizedArrayOutput { node_type, port, .. } => {
            JsonGeneratorLoadError::UnsizedArrayOutput { node_type, port }
        }
        P::UnsizedTexture3DOutput { node_type, port, .. } => {
            JsonGeneratorLoadError::UnsizedTexture3DOutput { node_type, port }
        }
        P::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        } => JsonGeneratorLoadError::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        },
    }
}

/// Structured error variants the chain runner produces. Every variant
/// carries the affected effect's identity so the future editor surface
/// can highlight the right card / node. Today this drives the
/// consistent `[chain-error]` terminal log; tomorrow it's the data
/// the editor reads via [`PresetRuntime::errors`].
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
    /// BUG-104 Part 5(b): a `node.switch_value` whose `selector` derives
    /// from a trigger source shadows a continuously-bound producer on one
    /// of its `in_N` branches instead of composing onto it — the class of
    /// bug that made Lissajous's Freq X/Y Rate faders go dead (and stay
    /// dead) while a Clip Trigger was active. Detected by
    /// [`crate::node_graph::trigger_shadow_lint`] on every generator
    /// (re)build in [`PresetRuntime::from_def`] — the same warning reaches
    /// the editor (via [`PresetRuntime::errors`]), an MCP-driven mutation,
    /// or an agent-authored graph, since all three funnel through the same
    /// build path. Not a build failure — the graph still runs; this is a
    /// severity-warning entry surfaced through the existing structured
    /// diagnostic channel rather than a new one.
    TriggerShadowsContinuousBinding {
        node_id: String,
        port: String,
        shadowed_source: String,
    },
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
            Self::TriggerShadowsContinuousBinding {
                node_id,
                port,
                shadowed_source,
            } => write!(
                f,
                "node `{node_id}`.{port}: trigger-driven switch_value shadows a continuous \
                 binding at {shadowed_source} — a card fader feeding that binding will go dead \
                 while the trigger is active (BUG-104). Compose instead of replace (see \
                 docs/DECOMPOSING_GENERATORS.md §4.1's trigger_modulate idiom: switch_value with \
                 an identity default on the idle branch + a downstream math node), or if this is a \
                 genuine discrete selector, add it to trigger_shadow_lint::DISCRETE_REPLACE_ALLOWLIST \
                 and record the decision in this preset's description."
            ),
        }
    }
}

impl std::error::Error for ChainError {}

/// Push a [`ChainError`] onto an accumulator and emit one consistent
/// `[chain-error]` line. Replaces the scattered `eprintln!` calls that
/// previously lacked structure — same data lands in the log, plus
/// it's now reachable through [`PresetRuntime::errors`] for the editor.
fn record_chain_error(errors: &mut Vec<ChainError>, err: ChainError) {
    eprintln!("[chain-error] {err}");
    errors.push(err);
}

struct EffectSlot {
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
    /// [`PresetRuntime::set_preview_target`] to substitute the group's primary
    /// texture-output producer. The third element is that output's interface
    /// port name (`forceField`), which drives the preview encoding. Empty for
    /// groupless effects.
    group_preview_map: Vec<(NodeId, NodeId, String)>,
    /// Propagated preview data-kind per node `NodeId`, computed once at chain
    /// build from the flattened def via [`PreviewEncoding::propagate`]. Lets the
    /// node-output preview follow the *data*: a Gaussian Blur whose input was a
    /// force field resolves to `VectorField` here even though the blur's own
    /// descriptor says nothing. [`PresetRuntime::set_preview_target`] looks the
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
    bound: BoundGraph,
    /// Last seen `PresetInstance.graph_version` for the user tail. User
    /// bindings live in the per-instance graph now, so a binding add /
    /// remove / reshape bumps the graph version. When the live effect's
    /// version differs, the per-frame apply path re-hydrates the user tail
    /// of [`Self::bindings`] from the synthesized binding list before
    /// applying.
    user_bindings_version: u32,
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
    def_content_key: u64,
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
    card_prefix: String,
}

/// The active (enabled, group-enabled) effects of a chain, with their original
/// indices into `effects`. Shared between the chain build and the project-load
/// segment prewarm so both walk the identical card list.
fn chain_active_effects<'a>(
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

/// Chain-fusion segment eligibility for one card (docs/CHAIN_FUSION_DESIGN.md).
/// Shared between the chain build and the project-load prewarm so the two can
/// never disagree about what forms a segment.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SegmentMember {
    /// Never joins or spans a segment (watched / grouped / stateful /
    /// string-bound / no view).
    Boundary,
    /// Fusable segment member.
    Fuse,
    /// Currently skipped — splices nothing; transparent to a run.
    Transparent,
}

fn classify_segment_member(
    fx: &PresetInstance,
    preview_effect: Option<&EffectId>,
    primitives: &PrimitiveRegistry,
) -> SegmentMember {
    if preview_effect == Some(&fx.id) || fx.group_id.is_some() {
        return SegmentMember::Boundary;
    }
    // `docs/DEPTH_RELIGHT_DESIGN.md` P5: relight augmentation is per-def
    // (`splice_def_into_chain`'s `relight` argument is a single def's
    // `RelightParams`), and a fused segment's compiled `view.def` already
    // folds multiple cards' shapes into one kernel — there's no per-member
    // slot to splice a second card's template into. A relight-on card stays
    // a segment `Boundary` so it renders through the ordinary per-card path
    // (where relight is fully wired) instead of silently losing its toggle
    // inside a fused run.
    if fx.relight {
        return SegmentMember::Boundary;
    }
    let Some(view) = loaded_preset_view_by_id(fx.effect_type()) else {
        return SegmentMember::Boundary;
    };
    if is_skipped_for(view.skip_mode, &view.type_id, fx) {
        return SegmentMember::Transparent;
    }
    if view
        .canonical_def
        .preset_metadata
        .as_ref()
        .is_some_and(|m| !m.string_bindings.is_empty())
    {
        return SegmentMember::Boundary;
    }
    let effective = fx.graph.as_ref().unwrap_or(&view.canonical_def);
    if crate::node_graph::freeze::segment::def_is_segment_stateless(effective, primitives) {
        SegmentMember::Fuse
    } else {
        SegmentMember::Boundary
    }
}

/// Scan one maximal segment run starting at `i` (caller guarantees
/// `members[i] == Fuse`): returns `(j, fuse_idxs)` — the exclusive end after
/// trimming trailing transparents, and the fusable indices within `[i, j)`.
fn segment_run(members: &[SegmentMember], i: usize) -> (usize, Vec<usize>) {
    let mut j = i;
    while j < members.len() && members[j] != SegmentMember::Boundary {
        j += 1;
    }
    // Trim trailing transparents back into plain cards.
    while j > i && members[j - 1] == SegmentMember::Transparent {
        j -= 1;
    }
    let fuse_idxs = (i..j).filter(|&k| members[k] == SegmentMember::Fuse).collect();
    (j, fuse_idxs)
}

/// Project-load PREWARM (chain fusion): walk one chain's effect list with the
/// exact build-time segmentation and enqueue the background compile for every
/// segment that would form — so the first dispatch of a scene finds its fused
/// view Ready instead of rendering the opening seconds of the show per-card.
/// Enqueue-only and content-keyed: results land through the normal worker →
/// pump → generation-bump → rebuild path, duplicates dedupe in the pending
/// set, and an already-cached segment is a no-op. `preview_effect` is `None` —
/// nothing is watched at load.
pub fn prewarm_chain_segments(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    primitives: &PrimitiveRegistry,
) {
    use crate::node_graph::freeze::install as freeze_install;
    if !freeze_install::chain_fusion_enabled() {
        return;
    }
    let active = chain_active_effects(effects, groups);
    let members: Vec<SegmentMember> = active
        .iter()
        .map(|(_, fx)| classify_segment_member(fx, None, primitives))
        .collect();
    let mut i = 0;
    while i < active.len() {
        if members[i] != SegmentMember::Fuse {
            i += 1;
            continue;
        }
        let (j, fuse_idxs) = segment_run(&members, i);
        if fuse_idxs.len() >= 2 {
            let cards: Vec<(&EffectGraphDef, &'static LoadedPresetView)> = fuse_idxs
                .iter()
                .map(|&k| {
                    let fx = active[k].1;
                    let view = loaded_preset_view_by_id(fx.effect_type())
                        .expect("eligibility implies view");
                    (fx.graph.as_ref().unwrap_or(&view.canonical_def), view)
                })
                .collect();
            let _ = freeze_install::fused_segment_view_for(&cards);
        }
        i = j.max(i + 1);
    }
}

/// Project-wide segment prewarm: enqueue background segment compiles for the
/// master chain and every layer chain in `project`. Call once at project load
/// (content thread) — by the time a scene's chain first dispatches, its fused
/// segments are compiled and gate-measured instead of the show opening
/// per-card. Builds its own registry (load-time, off the hot path).
pub fn prewarm_project_chain_segments(project: &manifold_core::project::Project) {
    use crate::node_graph::freeze::install as freeze_install;
    if !freeze_install::chain_fusion_enabled() {
        return;
    }
    let primitives = PrimitiveRegistry::with_builtin();
    static EMPTY_GROUPS: Vec<EffectGroup> = Vec::new();
    prewarm_chain_segments(
        &project.settings.master_effects,
        project.settings.master_effect_groups.as_ref().unwrap_or(&EMPTY_GROUPS),
        &primitives,
    );
    for layer in &project.timeline.layers {
        if let Some(effects) = layer.effects.as_ref() {
            prewarm_chain_segments(
                effects,
                layer.effect_groups.as_ref().unwrap_or(&EMPTY_GROUPS),
                &primitives,
            );
        }
    }
}

/// BUG-080 seam: a provisional manifest (built against an incomplete
/// registry, not yet reconciled) reaching chain build means a load/ingest
/// path skipped `reconcile_param_manifests()`. Loud in dev (panics), throttled
/// once per instance in release. Extracted so the seam behavior is directly
/// unit-testable without driving a full chain build — see
/// `docs/PARAM_MANIFEST_GATE_DESIGN.md` D2, INV-1.
fn assert_manifest_gate(fx: &PresetInstance) {
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
        // The runtime this build replaces, when the dispatcher is rebuilding
        // an existing chain. Cards whose content is unchanged harvest their
        // node state (impl instances + StateStore buckets) from it, so a
        // reorder / add / bypass / editor-close / fused-segment swap-in never
        // resets a sim or a feedback trail — state identity is the card, not
        // the chain position (docs/CHAIN_FUSION_DESIGN.md §5). `None` on
        // first build; skipped automatically when dimensions changed.
        prior: Option<&mut Self>,
    ) -> Option<Self> {
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
                let cards: Vec<(&EffectGraphDef, &'static LoadedPresetView)> = fuse_idxs
                    .iter()
                    .map(|&k| {
                        let fx = active_effects[k].1;
                        let view = loaded_preset_view_by_id(fx.effect_type())
                            .expect("eligibility implies view");
                        (fx.graph.as_ref().unwrap_or(&view.canonical_def), view)
                    })
                    .collect();
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
            // `docs/DEPTH_RELIGHT_DESIGN.md` P5: a relight-on card is never
            // fused — `fused_view_for` compiles `effective_def` down to one
            // opaque kernel with no notion of `relight_augment`'s template,
            // so fusing would silently drop the "3D Shading" toggle instead
            // of rendering it. Same veto as the segment path
            // (`classify_segment_member`) at single-card granularity.
            let fused_view: Option<std::sync::Arc<LoadedPresetView>> =
                if !fx.relight
                    && crate::node_graph::freeze::install::should_render_fused(
                        preview_effect == Some(&fx.id),
                    )
                {
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
            let view: &LoadedPresetView = fused_view.as_deref().unwrap_or(base_view);
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
                &view.canonical_def
            } else if let Some(def) = &fx.graph {
                def
            } else {
                &view.canonical_def
            };
            // The "3D Shading" toggle (`docs/DEPTH_RELIGHT_DESIGN.md` P5):
            // relight-on cards are excluded from fusion eligibility above
            // (`classify_segment_member`) and from the single-card fusion
            // gate too — a relight-on card is never `fused_view.is_some()`
            // here in practice since fusion itself doesn't special-case
            // relight, but keeping the toggle live on the unfused splice
            // path is the one that matters: the template needs to see (and
            // rebuild against) the actual node graph, not a fused kernel
            // that doesn't know about `rl_` nodes.
            let relight_params =
                fx.relight.then_some(&fx.relight_params);
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

        let mut runtime = Self {
            graph,
            plan,
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
            // If the preset includes a `system.generator_input` node,
            // push every frame-context scalar (time / beat / aspect /
            // output dims / trigger_count / anim_progress) into its
            // params. The standard port-shadows-param machinery
            // propagates these to inner primitives via scalar wires —
            // same surface generators have, no per-effect Rust code.
            // §8 D5 (2026-07-07): `trigger_count` used to stay pinned at
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
            // PresetRuntime fast path can throttle correctly. Was
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
            .or_else(|| n.node.outputs().first().map(|p| p.name.as_ref()))
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

    /// Live (post-binding-apply, post-modulation) scalar param values for every
    /// node of `effect_id`, keyed by stable [`NodeId`]. Lets the editor canvas
    /// reflect what a card slider / driver / Ableton / envelope is doing to each
    /// inner knob *this frame*, instead of the frozen authoring def that the
    /// structural `from_def` snapshot carries (it only rebuilds on `graph_version`,
    /// so modulation never moved it). Card bindings apply via
    /// [`BoundGraph::apply`](crate::node_graph::BoundGraph) → `graph.set_param`,
    /// which writes the reshaped value straight into the node's param map, so
    /// reading it back here is exactly what the executor just ran with. Empty
    /// when this chain doesn't hold `effect_id`. Cheap: param names are
    /// `&'static`, so only the small `Vec`s allocate per frame.
    ///
    /// A param whose same-named input port carries a connected scalar wire
    /// reads [`Executor::live_scalar_input`](crate::node_graph::Executor::live_scalar_input)
    /// first — the executor's per-frame wire-value tap — falling back to the
    /// param map only when unwired. Mirrors
    /// [`EffectNodeContext::scalar_or_param`](crate::node_graph::effect_node::EffectNodeContext::scalar_or_param)'s
    /// port-shadows-param resolution order, so the editor's live value tap
    /// doesn't freeze on a wire-driven scalar param while the render keeps
    /// moving (PARAM_TWO_WAY_BINDING_DESIGN.md P2 D5).
    pub fn live_node_params(&self, effect_id: &EffectId) -> crate::node_graph::LiveNodeParams {
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        slot.node_map
            .iter()
            .filter_map(|(node_id, inst)| {
                let n = self.graph.get_node(*inst)?;
                let values = n
                    .node
                    .parameters()
                    .iter()
                    .map(|pd| {
                        let v = self
                            .executor
                            .live_scalar_input(*inst, pd.name.as_ref())
                            .unwrap_or_else(|| {
                                n.params
                                    .get(pd.name.as_ref())
                                    .map(crate::node_graph::param_default_to_f32)
                                    .unwrap_or_else(|| {
                                        crate::node_graph::param_default_to_f32(&pd.default)
                                    })
                            });
                        (crate::node_graph::intern_name(&pd.name), v)
                    })
                    .collect();
                Some((node_id.clone(), values))
            })
            .collect()
    }

    /// Generator convenience: a generator runtime holds exactly one effect (the
    /// whole generator), so its live params are [`Self::live_node_params`] for
    /// that single slot. Empty for an effect-chain runtime with no slots.
    pub fn live_node_params_watched(&self) -> crate::node_graph::LiveNodeParams {
        match self.effect_nodes.first() {
            Some(slot) => {
                let eid = slot.effect_id.clone();
                self.live_node_params(&eid)
            }
            None => Vec::new(),
        }
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
    /// This is the Cmd+D disk dump (whole graph); the editor thumbnail atlas
    /// uses [`Self::set_dump_visible`] instead (only the visible nodes).
    pub fn set_dump(&mut self, dump_effect: Option<&EffectId>) {
        let on =
            dump_effect.is_some_and(|eid| self.effect_nodes.iter().any(|s| &s.effect_id == eid));
        self.executor.set_dump_all(on);
    }

    /// Set (or clear) the continuous thumbnail-atlas dump for this chain —
    /// record only the nodes the editor canvas can currently show, resolved
    /// from their stable [`NodeId`]s to runtime instances via the owning slot's
    /// `node_map`. A `visible` id that names a selected group resolves to its
    /// primary-output producer via `group_preview_map`, mirroring
    /// [`Self::set_preview_target`]. `effect_id` selects the owning effect slot;
    /// pass `None` for a generator runtime (one graph, every slot eligible). A
    /// chain that doesn't hold the requested effect clears its set, so only the
    /// watched chain pays. Hidden / off-scope nodes are simply absent from the
    /// set, so they keep memoization and their textures recycle (sub-changes
    /// A + B).
    pub fn set_dump_visible(&mut self, effect_id: Option<&EffectId>, visible: &[NodeId]) {
        let mut set: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::new();
        let mut matched = effect_id.is_none();
        for slot in &self.effect_nodes {
            if let Some(eid) = effect_id {
                if &slot.effect_id != eid {
                    continue;
                }
                matched = true;
            }
            for nid in visible {
                if let Some((_, instance)) =
                    slot.node_map.iter().find(|(mapped, _)| mapped == nid)
                {
                    set.insert(*instance);
                } else if let Some((_, producer, _)) =
                    slot.group_preview_map.iter().find(|(group, _, _)| group == nid)
                    && let Some((_, instance)) =
                        slot.node_map.iter().find(|(mapped, _)| mapped == producer)
                {
                    set.insert(*instance);
                }
            }
        }
        self.executor.set_dump_set(if matched { Some(set) } else { None });
    }

    /// Clear any thumbnail-atlas dump set on this chain (atlas off, or this
    /// chain isn't the watched one).
    pub fn clear_dump_set(&mut self) {
        self.executor.set_dump_set(None);
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
        for (node, port, _res, tex) in self.executor.dump_resources() {
            // Only this effect's nodes (reverse-map runtime id → stable NodeId).
            let Some((node_id, _)) = slot.node_map.iter().find(|(_, niid)| niid == node) else {
                continue;
            };
            // Texture pinned at record time, immune to the end-of-frame swap.
            let Some(tex) = tex.as_ref() else {
                continue;
            };
            let type_id = self
                .graph
                .get_node(*node)
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

// ===========================================================================
// Generator construction + per-frame surface
// ===========================================================================
//
// A generator is the degenerate `PresetRuntime`: one preset graph (a single
// `EffectSlot` segment), no input texture (`PresetIo::Generate`), boundary
// `system.generator_input` + `system.final_output` nodes. It renders *into* a
// host-provided target texture rather than owning its output. The shared
// substrate (graph / plan / executor / state_store / preview / dump) is the
// same as the effect-chain path above.

impl PresetRuntime {
    /// Parse a generator-preset JSON string and compile it (mock backend —
    /// fine for unit tests; production uses [`Self::from_json_str_with_device`]).
    pub fn from_json_str(
        json: &str,
        registry: &PrimitiveRegistry,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        // Mock-backend/test convenience path: no per-instance manifest in scope,
        // so the reshape reads the def's own (fresh) `preset_metadata.params`.
        Self::from_def(doc, registry, None)
    }

    /// Build a generator from an already-parsed [`EffectGraphDef`]. Same path
    /// as [`Self::from_json_str`] minus the JSON parse step.
    ///
    /// `manifest` is the live per-instance [`ParamManifest`] (`Layer.gen_params
    /// .params`) when this build is a rebuild of an on-project generator, else
    /// `None` for a standalone build (thumbnails / `check_presets` /
    /// `freeze_profile` / gltf import / freeze proofs). When present, each
    /// param's reshape (min/max/curve/invert) is sourced from the manifest
    /// `spec` — the single authority (D4) — instead of the graph's
    /// `preset_metadata.params` shadow, which is only re-derived from the
    /// manifest at serialize time (D12) and is therefore stale between a
    /// calibration and the next save (BUG-078). `None` keeps reading the
    /// shadow, correct for a fresh-from-disk def whose shadow is accurate.
    pub fn from_def(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        if doc.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
            return Err(JsonGeneratorLoadError::Load(LoadError::UnsupportedVersion {
                found: doc.version,
                max: EFFECT_GRAPH_VERSION_WITH_METADATA,
            }));
        }

        let type_id_str: String = match doc.preset_metadata.as_ref() {
            Some(m) => m.id.as_str().to_string(),
            None => match doc.name.clone() {
                Some(n) => n,
                None => {
                    return Err(JsonGeneratorLoadError::Load(LoadError::InvalidWire {
                        wire_index: 0,
                        reason: "generator preset must declare either a top-level `name` or \
                                 `presetMetadata.id`"
                            .into(),
                    }));
                }
            },
        };
        let type_id = PresetTypeId::from_string(type_id_str);

        // Validate boundary-node presence on the JSON document BEFORE building
        // the runtime graph — `compile()` would fail with a less informative
        // `RequiredInputUnwired` on a missing FinalOutput-source wire.
        if !doc.nodes.iter().any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID) {
            return Err(JsonGeneratorLoadError::MissingGeneratorInput);
        }
        if !doc.nodes.iter().any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID) {
            return Err(JsonGeneratorLoadError::MissingFinalOutput);
        }

        // Capture the binding specs + outer-card param ids before `into_graph`
        // consumes `doc`. The id list resolves each binding's `source_index`
        // (which outer slider it draws from) — keyed by id rather than position
        // so a single slider can fan out to multiple inner-node params.
        let binding_specs: Vec<manifold_core::effect_graph_def::BindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.bindings.clone())
            .unwrap_or_default();
        // Per-param slider response (preset curve/invert/range), matching the
        // effect path's `ResolvedBinding::from_static` no-note reshape. Identity
        // for every shipped preset. `param_id -> (min, max, curve, invert)`.
        //
        // Base layer: the graph's `preset_metadata.params` shadow — correct for
        // a standalone build (`manifest = None`), whose def is fresh-from-disk.
        let mut param_reshape: ahash::AHashMap<
            String,
            (f32, f32, manifold_core::macro_bank::MacroCurve, bool),
        > = doc
            .preset_metadata
            .as_ref()
            .map(|m| {
                m.params
                    .iter()
                    .map(|p| (p.id.clone(), (p.min, p.max, p.curve, p.invert)))
                    .collect()
            })
            .unwrap_or_default();
        // BUG-078: the shadow above is derived from the per-instance manifest
        // only at serialize time (D12), so between a calibration and the next
        // save it carries the pre-calibration range. When the live manifest is
        // available (the generator_renderer rebuild path threads it here), its
        // `spec` is the authority for each param's reshape (D4) — overlay it,
        // manifest-wins-per-id, so a post-calibration structural rebuild honors
        // the fresh range/curve/invert. The effect path already does this via
        // `synth_user_binding` reading `self.params`; this is the generator's
        // equivalent for its shared (stock + user) binding path.
        if let Some(manifest) = manifest {
            for p in manifest.iter() {
                param_reshape.insert(
                    p.spec.id.clone(),
                    (p.spec.min, p.spec.max, p.spec.curve, p.spec.invert),
                );
            }
        }
        let string_binding_specs: Vec<manifold_core::effect_graph_def::StringBindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.string_bindings.clone())
            .unwrap_or_default();

        // Group → producer map for the node-output preview, captured before
        // `into_graph` flattens the groups away.
        let group_preview_map = manifold_core::flatten::group_output_producer_map(&doc);
        // Flattened once, shared by the node-output preview kind propagation
        // AND the BUG-104 trigger-shadow class check below — both need the
        // group-boundary-free view of the graph.
        let flat_doc = manifold_core::flatten::flatten_groups(&doc).ok();
        let preview_kinds = flat_doc
            .as_ref()
            .map(crate::node_graph::PreviewEncoding::propagate)
            .unwrap_or_default();
        let mut chain_errors: Vec<ChainError> = Vec::new();
        if let Some(flat) = flat_doc.as_ref() {
            for finding in crate::node_graph::trigger_shadow_lint::find_trigger_shadow_findings(flat)
            {
                if crate::node_graph::trigger_shadow_lint::is_allowlisted(
                    type_id.as_str(),
                    &finding.node_id,
                ) {
                    continue;
                }
                record_chain_error(
                    &mut chain_errors,
                    ChainError::TriggerShadowsContinuousBinding {
                        node_id: finding.node_id,
                        port: finding.port,
                        shadowed_source: finding.shadowed_source,
                    },
                );
            }
        }

        let mut graph = doc.into_graph(registry)?;
        let plan = compile(&graph)?;

        // Re-locate the boundary nodes by runtime id now that we have the live
        // graph.
        let generator_input_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingGeneratorInput)?;
        // BUG-125: `.find()` over the graph's unordered node map is only safe
        // when at most one node matches — count first so a second
        // `final_output` is rejected loudly at load instead of one of the
        // two being picked nondeterministically per process.
        let final_output_count = graph
            .nodes()
            .filter(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
            .count();
        if final_output_count > 1 {
            return Err(JsonGeneratorLoadError::MultipleFinalOutputs {
                count: final_output_count,
            });
        }
        let final_output_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;
        // Walk the plan for the FinalOutput step, pull its `in` input resource —
        // that's what the host pre-binds the target texture to.
        let final_output_input_resource = plan
            .steps()
            .iter()
            .find(|s| s.node == final_output_id)
            .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
            .map(|(_, res)| *res)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;

        // Resolve the binding specs against the live graph into the SHARED
        // `ResolvedBinding` type — the same one the effect chain uses — so the
        // per-frame apply runs through `BoundGraph::apply` (skip-on-unchanged
        // cache + structured error logging). Bindings whose node id / param
        // doesn't resolve are warned + dropped.
        use manifold_core::effect_graph_def::BindingTarget;
        let bindings: Vec<ResolvedBinding> = binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    let inst_id = graph.instance_by_node_id(node_id)?;
                    let inst = graph.get_node(inst_id)?;
                    let static_param = inst
                        .node
                        .parameters()
                        .iter()
                        .map(|p| crate::node_graph::intern_name(&p.name))
                        .find(|name| *name == param.as_str())
                        .or_else(|| {
                            log::warn!(
                                "PresetRuntime(gen): binding id `{}` targets node `{node_id}`.`{param}` \
                                 but that param doesn't exist on the node — dropping binding.",
                                b.id,
                            );
                            None
                        })?;
                    let (rmin, rmax, rcurve, rinvert) = param_reshape
                        .get(b.id.as_str())
                        .copied()
                        .unwrap_or((0.0, 1.0, manifold_core::macro_bank::MacroCurve::Linear, false));
                    let reshape = crate::node_graph::Reshape::from_preset_response(
                        rmin, rmax, rcurve, rinvert, b.scale, b.offset,
                    );
                    Some(ResolvedBinding::assemble(
                        std::borrow::Cow::Owned(b.id.clone()),
                        std::borrow::Cow::Owned(b.label.clone()),
                        b.default_value,
                        ResolvedTarget::Node {
                            node: inst_id,
                            param: std::borrow::Cow::Borrowed(static_param),
                        },
                        b.convert,
                        if b.user_added {
                            BindingSource::User
                        } else {
                            BindingSource::Static
                        },
                        std::borrow::Cow::Owned(b.id.clone()),
                        reshape,
                        false,
                    ))
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // Hand the resolved bindings to the shared `BoundGraph` (seeds the
        // skip-cache + plants each binding's declared default).
        let bound = BoundGraph::new(bindings, &mut graph);
        // Stable NodeId → live instance over the whole graph.
        let node_map: Vec<(NodeId, NodeInstanceId)> = graph
            .nodes()
            .filter(|n| !n.node_id.as_str().is_empty())
            .map(|n| (n.node_id.clone(), n.id))
            .collect();

        let string_bindings: Vec<StringBindingResolution> = string_binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    let inst_id = graph.instance_by_node_id(node_id)?;
                    Some(StringBindingResolution {
                        target_node: inst_id,
                        target_param: param.clone(),
                        source_key: b.id.clone(),
                        default: b.default_value.clone(),
                        def_value: flat_doc
                            .as_ref()
                            .and_then(|flat| def_string_param_value(flat, node_id, param)),
                    })
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // The generator's single segment. Most `EffectSlot` fields are inert
        // for a generator (it has no chain index, no per-frame user-tail
        // rehydrate — its host rebuilds on structure change); the live ones are
        // `bound`, `node_map`, `generator_input_node`, and the preview maps.
        let segment = EffectSlot {
            effect_id: EffectId::default(),
            effect_type: type_id.clone(),
            legacy_index: 0,
            handles: Vec::new(),
            node_map,
            group_preview_map,
            preview_kinds,
            applied_graph_version: 0,
            bound,
            user_bindings_version: 0,
            // Generators rebuild through their own registry lifecycle, not the
            // chain dispatcher's prior-runtime handoff — no harvest key.
            def_content_key: 0,
            generator_input_node: Some(generator_input_id),
            card_prefix: String::new(),
        };

        let mut g = Self {
            graph,
            plan,
            executor: Executor::with_mock(),
            effect_nodes: vec![segment],
            group_mix_nodes: Vec::new(),
            io: PresetIo::Generate {
                generator_input_id,
                final_output_input_resource,
                final_output_slot: None,
            },
            width: 0,
            height: 0,
            topology_hash: 0,
            built_generation: 0,
            pending_segments: false,
            built_segment_generation: 0,
            state_store: StateStore::new(),
            errors: chain_errors,
            preview_encoding: crate::node_graph::PreviewEncoding::default(),
            type_id: Some(type_id),
            target_format: None,
            string_bindings,
        };
        g.apply_string_defaults();
        Ok(g)
    }

    /// Parse + compile + wire to a real [`MetalBackend`] for production
    /// rendering. Pre-binds a 1×1 placeholder at the FinalOutput-source slot so
    /// per-frame `render()` only swaps the borrowed texture (no hot-path alloc).
    pub fn from_json_str_with_device(
        json: &str,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        Self::from_def_with_device(doc, registry, device, width, height, format, manifest)
    }

    /// Same as [`Self::from_json_str_with_device`] but skips the JSON parse.
    /// `manifest` follows the [`Self::from_def`] contract: the live per-instance
    /// [`ParamManifest`] on a project-generator rebuild, `None` standalone.
    pub fn from_def_with_device(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let mut g = Self::from_def(doc, registry, manifest)?;
        g.width = width;
        g.height = height;
        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), width, height, format);
        let PresetIo::Generate {
            final_output_input_resource,
            ..
        } = g.io
        else {
            unreachable!("from_def always produces Generate IO");
        };
        // Pre-bind a 1×1 placeholder at the FinalOutput-source slot so the slot
        // exists across frames; `install_target` swaps in the host's real target
        // via `replace_texture_2d` each render call.
        let placeholder =
            RenderTarget::new(&device, 1, 1, format, "preset_runtime_target_owner");
        let slot = backend.pre_bind_texture_2d(final_output_input_resource, placeholder);
        if let PresetIo::Generate {
            final_output_slot, ..
        } = &mut g.io
        {
            *final_output_slot = Some(slot);
        }
        g.target_format = Some(format);

        // Pre-allocate every Array<T> buffer + Texture3D volume the compiled
        // plan declares, then run the post-allocation audit — the same shared
        // pipeline the effect chain uses.
        crate::node_graph::pre_allocate_resources(&g.graph, &g.plan, &device, &mut backend)
            .map_err(generator_error_from_prealloc)?;

        g.executor = Executor::new(Box::new(backend));
        Ok(g)
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

    /// Test-only handle to the executor's backend (post-rebuild canvas-dim
    /// assertions). Not on the hot path.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn backend_for_test(&self) -> &dyn crate::node_graph::Backend {
        self.executor.backend()
    }

    /// Replace the internal executor — used when a host wires a real backend
    /// after a mock-backend construction.
    pub fn set_executor(&mut self, executor: Executor) {
        self.executor = executor;
    }

    /// Enable/disable one-shot "dump every output" mode on the executor
    /// (preserve every Texture2D output for one frame). Generator path; the
    /// effect chain uses [`Self::set_dump`] (gated by effect id).
    pub fn set_dump_all(&mut self, on: bool) {
        self.executor.set_dump_all(on);
    }

    /// Enable/disable per-step attribution profiling on this chain's executor
    /// (PERF_BUDGET_GATE_DESIGN P2 / D6). Off by default — one branch per
    /// step on the live path.
    pub fn set_profiling(&mut self, on: bool) {
        self.executor.set_profiling(on);
    }

    /// Set this chain's instance identity for profiled tags (D6 correction):
    /// `fx:{layer_id}`, `gen:{layer_id}`, `master`, `led:{...}`. Called by the
    /// owning compositor/generator-renderer at chain-insertion time.
    pub fn set_profile_scope(&mut self, scope: &str) {
        self.executor.set_profile_scope(scope);
    }

    /// Drain this chain's per-step CPU profiles recorded on the last profiled
    /// frame (each entry's `tag` is the scoped GPU-span join key).
    pub fn take_step_profiles(&mut self) -> Vec<crate::node_graph::StepProfile> {
        self.executor.take_step_profiles()
    }

    /// Update the `system.generator_input` node's per-frame context. No-op on
    /// an effect-chain runtime.
    #[allow(clippy::too_many_arguments)]
    pub fn set_frame_context(
        &mut self,
        time: f32,
        beat: f32,
        aspect: f32,
        trigger_count: f32,
        anim_progress: f32,
        output_width: f32,
        output_height: f32,
    ) {
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

    /// Push the host's per-clip string overrides through the preset's
    /// `stringBindings` to the matching inner-node String params. Only keys
    /// PRESENT in `values` are written — an absent key leaves the live node
    /// param untouched (BUG-182: the previous fall-back-to-default behavior
    /// re-asserted the binding's declared default every frame, so a file path
    /// set directly on the node — e.g. `node.hdri_source`'s `path` via the
    /// graph editor's picker — was silently overwritten by the card's empty
    /// `hdri_file` default before the next frame ran). Defaults are seeded
    /// once at construction by [`Self::apply_string_defaults`], so absent
    /// keys still start from the binding default on a fresh runtime.
    pub fn apply_string_params(
        &mut self,
        values: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        let Some(values) = values else { return };
        for binding in &self.string_bindings {
            let Some(v) = values.get(binding.source_key.as_str()) else {
                continue;
            };
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(v.clone())),
            );
        }
    }

    /// Seed every string binding's value once at construction, before the
    /// host's first `set_string_params` call. Precedence (BUG-182): the def
    /// node's OWN param value (`binding.def_value`, captured from the def at
    /// resolution time) wins over the binding's declared default, so a
    /// def-baked value — e.g. a file path set directly on the node in the
    /// graph editor — survives construction. Host values pushed later via
    /// [`Self::apply_string_params`] override either. (The live graph can't
    /// be consulted for this distinction: `Graph::add_node` pre-populates
    /// every declared param with its primitive default, so presence there
    /// says nothing about whether the DEF set the param.)
    fn apply_string_defaults(&mut self) {
        for binding in &self.string_bindings {
            let seed = binding.def_value.as_ref().unwrap_or(&binding.default);
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(seed.clone())),
            );
        }
    }

    pub fn set_string_params(
        &mut self,
        params: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        self.apply_string_params(params);
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

    /// Run one frame against the configured executor (mock-backend test path).
    pub fn execute_frame(&mut self, time: FrameTime) {
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
        self.set_frame_context(
            ctx.time as f32,
            ctx.beat as f32,
            ctx.aspect,
            ctx.trigger_count as f32,
            ctx.anim_progress,
            ctx.output_width as f32,
            ctx.output_height as f32,
        );

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

    /// Aim the authoring-time node-output preview at the editor's stable
    /// [`NodeId`](manifold_core::NodeId) within this generator, or clear it. A
    /// selected *group* container resolves to its primary texture-output
    /// producer (groups flatten away, so a direct lookup misses).
    pub fn set_preview_node(&mut self, node_id: Option<&manifold_core::NodeId>) {
        use crate::node_graph::PreviewEncoding;
        let mut encoding = PreviewEncoding::Color;
        // The generator's single segment carries the preview maps.
        let target = node_id.and_then(|nid| {
            // Direct node hit.
            if let Some(inst) = self.graph.instance_by_node_id(nid) {
                encoding = self
                    .effect_nodes
                    .first()
                    .and_then(|s| s.preview_kinds.get(nid).copied())
                    .unwrap_or_else(|| self.encoding_for_instance(inst, None));
                return Some(inst);
            }
            // Group container: capture its producer.
            let seg = self.effect_nodes.first()?;
            if let Some((_, producer, port)) =
                seg.group_preview_map.iter().find(|(group, _, _)| group == nid)
                && let Some(inst) = self.graph.instance_by_node_id(producer)
            {
                encoding = PreviewEncoding::from_port_name(port)
                    .or_else(|| seg.preview_kinds.get(producer).copied())
                    .unwrap_or(PreviewEncoding::Color);
                return Some(inst);
            }
            None
        });
        self.preview_encoding = encoding;
        self.executor.set_preview_target(target);
    }

    /// After a `render` with dump mode on, every captured Texture2D output as
    /// `(node_id, port, type_id, texture)` — the generator's whole pipeline (no
    /// per-effect filter, unlike the chain's [`Self::dump_textures`]).
    pub fn dump_textures_all(&self) -> Vec<(String, String, String, &GpuTexture)> {
        let mut out = Vec::new();
        for (node, port, _res, tex) in self.executor.dump_resources() {
            // Texture pinned at record time, immune to the end-of-frame swap.
            let Some(tex) = tex.as_ref() else {
                continue;
            };
            let (name, type_id) = self
                .graph
                .get_node(*node)
                .map(|inst| {
                    (
                        inst.node_id.to_string(),
                        inst.node.type_id().as_str().to_string(),
                    )
                })
                .unwrap_or_default();
            out.push((name, port.to_string(), type_id, tex));
        }
        out
    }

    /// Whole-graph `Array` dump (generator path) — the array counterpart of
    /// [`Self::dump_textures_all`].
    pub fn dump_arrays_all(&self) -> Vec<crate::compositor::ArrayDump<'_>> {
        use crate::node_graph::ports::{ChannelElementType, PortType, std430_layout};
        let kind = |t: ChannelElementType| match t {
            ChannelElementType::F32 => "f32",
            ChannelElementType::I32 => "i32",
            ChannelElementType::U32 => "u32",
            ChannelElementType::Vec2F => "vec2f",
            ChannelElementType::Vec3F => "vec3f",
            ChannelElementType::Vec4F => "vec4f",
        };
        let mut out = Vec::new();
        for &(node, port, res) in self.executor.dump_array_resources() {
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
            let (name, type_id) = self
                .graph
                .get_node(node)
                .map(|inst| {
                    (
                        inst.node_id.to_string(),
                        inst.node.type_id().as_str().to_string(),
                    )
                })
                .unwrap_or_default();
            out.push(crate::compositor::ArrayDump {
                name,
                port: port.to_string(),
                type_id,
                buffer,
                item_size: at.item_size,
                fields,
            });
        }
        out
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
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5): the toggle and
        // its knobs change what `splice_def_into_chain` builds without
        // touching `fx.graph` or `graph_structure_version` (the template is
        // synthesized at splice time, not authored into the def) — fold
        // them into the rebuild key directly so a toggle flip or a knob
        // drag rebuilds this chain.
        fx.relight.hash(&mut h);
        if fx.relight {
            fx.relight_params.light_x.to_bits().hash(&mut h);
            fx.relight_params.light_y.to_bits().hash(&mut h);
            fx.relight_params.relief.to_bits().hash(&mut h);
            fx.relight_params.ao_intensity.to_bits().hash(&mut h);
            fx.relight_params.shadow_softness.to_bits().hash(&mut h);
            fx.relight_params.gain.to_bits().hash(&mut h);
            fx.relight_params.height_from.hash(&mut h);
        }
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
    /// Allocation dims per slot, indexed by `Slot.0`. Canvas-sized for the
    /// shared ping-pong slots; dedicated slots for held/persistent
    /// resources take the resource's resolved dims (a 256×1 LUT strip must
    /// not pin a canvas-sized texture for the chain's lifetime).
    slot_dims: Vec<(u32, u32)>,
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
fn assign_texture2d_slots(
    plan: &ExecutionPlan,
    source_resource: ResourceId,
    canvas_dims: (u32, u32),
) -> SlotAssignment {
    let mut resource_to_slot: AHashMap<ResourceId, Slot> = AHashMap::default();
    let source_slot = Slot(0);
    resource_to_slot.insert(source_resource, source_slot);
    let mut next_slot: u32 = 1;
    let mut slot_dims: Vec<(u32, u32)> = vec![canvas_dims];

    // Pre-allocate dedicated slots for every persistent AND held
    // Texture2D resource BEFORE the topological walk. These slots stay
    // out of the free pool for the rest of the simulation. Persistent:
    // the feedback loop's producer/consumer must share the carry-over
    // texture without any intermediate write aliasing it. Held (memo-
    // latched LUTs etc.): the executor serves the latched write on
    // every later frame while upstream transient steps keep re-running
    // — a shared slot would be stomped each frame (the 2026-06 Infrared
    // → QuadMirror blackout). Dedicated slots are allocated at the
    // resource's RESOLVED dims, so a 256×1 LUT strip costs 256×1, not
    // a canvas-sized texture.
    let dedicated_set: std::collections::HashSet<ResourceId> = plan
        .persistent_resources()
        .iter()
        .chain(plan.held_resources())
        .filter(|&&res_id| {
            res_id != source_resource
                && plan
                    .resource_type(res_id)
                    .map(|ty| ty.is_texture_2d())
                    .unwrap_or(false)
        })
        .copied()
        .collect();
    for &res_id in &dedicated_set {
        let slot = Slot(next_slot);
        next_slot += 1;
        slot_dims.push(crate::node_graph::execution::resolve_dims(
            plan, res_id, canvas_dims,
        ));
        resource_to_slot.insert(res_id, slot);
    }

    let mut free_pool: Vec<Slot> = Vec::new();

    for step in plan.steps() {
        // Acquire output slots — pop from free pool or grow.
        for &(_, res_id) in &step.outputs {
            if res_id == source_resource {
                continue;
            }
            if dedicated_set.contains(&res_id) {
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
                slot_dims.push(canvas_dims);
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
            if dedicated_set.contains(&res_id) {
                // Dedicated (persistent/held) slots never enter the free
                // pool. (Compile-time invariant: neither kind appears in
                // any step's `free_after` — this guard is defensive.)
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

    debug_assert_eq!(slot_dims.len(), next_slot as usize);
    SlotAssignment {
        resource_to_slot,
        source_slot,
        slot_count: next_slot,
        slot_dims,
    }
}

// (`pre_allocate_array_buffers_effect` was the stop-gap shim added in
// commit 3500e7a7 to fix the Blob Track drift bug. Both callers now
// route through `node_graph::graph_loader::pre_allocate_resources`
// which adds Texture3D + canvas-sized + post-allocation audit.)

#[cfg(all(test, feature = "gpu-proofs"))]
mod multi_segment_tests {
    //! Regression tests for the multi-segment wet/dry group support in
    //! `PresetRuntime::try_build`. A "multi-segment" group is one whose
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
        manifold_core::preset_definition_registry::create_default(&ty)
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
            PresetRuntime::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None, None);

        let cg = result.expect(
            "PresetRuntime should build for a non-contiguous wet/dry group \
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
            PresetRuntime::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None, None);

        let cg = result.expect("PresetRuntime should build for contiguous group");
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

        let result = PresetRuntime::try_build(
            &[e1, e2, e3, e4, e5],
            &[g1],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
            None,
        );

        let cg = result.expect("PresetRuntime should build for three-segment group");
        assert_eq!(cg.group_mix_nodes.len(), 3);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
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
        manifold_core::preset_definition_registry::create_default(&ty)
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

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
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
        manifold_core::preset_definition_registry::create_default(&ty)
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
        fx.set_base_param("amount", 0.0);

        let hash_at_zero = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        fx.set_base_param("amount", 0.5);
        let hash_at_half = compute_topology_hash(&[fx], &[], 256, 256, None);

        assert_ne!(
            hash_at_zero, hash_at_half,
            "topology hash must change when an effect's SkipMode::OnZero \
             predicate flips — otherwise the chain doesn't rebuild and \
             the user has to toggle enabled to bring the effect into the \
             graph. See the doc-comment on `compute_topology_hash`."
        );
    }

    #[cfg(feature = "gpu-proofs")]
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
        let cg_on = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
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
        let cg_off = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None);
        assert!(
            cg_off.is_none(),
            "Disabled effect must be filtered out of active_effects — got a chain with effects when it should be empty",
        );
    }

    /// `docs/DEPTH_RELIGHT_DESIGN.md` P5, full loop: flip
    /// `PresetInstance::relight` and rebuild the SAME production path
    /// (`try_build` → `compute_topology_hash`) real `EditingService`
    /// commands drive — `manifold-editing`'s
    /// `toggle_relight_undo_roundtrip` (command_roundtrips.rs) proves the
    /// command correctly flips this same field through undo/redo;
    /// `manifold-renderer` can't depend on `manifold-editing` (crate-graph
    /// direction), so this half of the loop proves the OTHER end: the
    /// renderer reads that field, mints deterministic `rl_`-prefixed nodes
    /// when it's on, and the topology hash changes so a toggle actually
    /// rebuilds — then removes them cleanly when toggled back off.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn toggling_relight_adds_and_removes_rl_nodes_on_rebuild() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let lambert_id = manifold_core::NodeId::new("rl_lambert");

        let mut fx = make_default(PresetTypeId::MIRROR);
        let hash_off = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_off = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Mirror chain builds with relight off");
        assert!(
            cg_off.graph.instance_by_node_id(&lambert_id).is_none(),
            "relight off must NOT contain the rl_lambert template node",
        );

        fx.relight = true;
        let hash_on = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        assert_ne!(
            hash_off, hash_on,
            "toggling relight MUST change the topology hash — otherwise the \
             chain never rebuilds and the toggle appears dead.",
        );
        let cg_on = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Mirror chain builds with relight on");
        assert!(
            cg_on.graph.instance_by_node_id(&lambert_id).is_some(),
            "relight on must splice the rl_lambert template node into the built chain",
        );

        // Toggle back off: the rebuilt chain must lose the template again —
        // proves this isn't a one-way sticky augmentation.
        fx.relight = false;
        let cg_off_again =
            PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
                .expect("Mirror chain builds with relight off again");
        assert!(
            cg_off_again.graph.instance_by_node_id(&lambert_id).is_none(),
            "toggling relight back off must remove the rl_ template nodes on rebuild",
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

#[cfg(all(test, feature = "gpu-proofs"))]
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
        PresetInstance, UserParamBinding, ParamConvert,
    };


    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// Set an existing manifest param's live + base value by id, marking it
    /// exposed — the id-keyed replacement for the old positional
    /// `fx.param_values[i] = ParamSlot::exposed(v)` write.
    fn set_slot(fx: &mut PresetInstance, id: &str, value: f32) {
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param `{id}` exists in the manifest"));
        p.value = value;
        p.base = value;
        p.exposed = true;
    }

    /// Clone the canonical preset def for `ty` and set a non-identity
    /// `scale` on the named card binding's [`BindingDef`] — the post-note
    /// home for a per-instance reshape (the deleted `ParamMapping` note's
    /// scale folded onto the binding spec). Returns the divergent def for
    /// the caller to hang on `fx.graph`.
    fn def_with_binding_scale(
        ty: PresetTypeId,
        binding_id: &str,
        scale: f32,
    ) -> manifold_core::effect_graph_def::EffectGraphDef {
        let mut def = (*loaded_preset_view_by_id(&ty)
            .expect("preset view exists for type")
            .canonical_def)
            .clone();
        let meta = def
            .preset_metadata
            .as_mut()
            .expect("preset carries presetMetadata");
        let binding = meta
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
            .expect("named card binding exists");
        binding.scale = scale;
        def
    }

    fn affine_scale(cg: &PresetRuntime, slot: &EffectSlot) -> ParamValue {
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

    /// Core model proof: a per-instance reshape (now a `scale` on the
    /// card binding's [`BindingDef`] in the instance's own graph, after
    /// the `ParamMapping` note was deleted) reshapes what the inner node
    /// sees (`zoom` → `affine.scale`), while the param's VALUE SLOT stays
    /// byte-identical — the load-bearing invariant for the live rig
    /// (Ableton / drivers / OSC / envelopes write that slot, untouched).
    #[test]
    fn stock_param_reshape_changes_inner_node_without_touching_the_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        // Mirror the per-frame apply `run()` performs: push the live
        // `params` manifest through the slot's bindings into the graph.
        fn apply(cg: &mut PresetRuntime, values: &ParamManifest) {
            let slot = &mut cg.effect_nodes[0];
            slot.bound.apply(&mut cg.graph, values);
        }

        // Control: same effect, zoom = 0.3, identity binding → inner sees 0.3.
        let mut control = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_slot(&mut control, "zoom", 0.3);
        // Build unfused (pass the effect as the watched preview) so the inner
        // affine node survives for inspection — region fusion would otherwise
        // fold it into a single kernel and the handle would vanish.
        let mut cg0 =
            PresetRuntime::try_build(std::slice::from_ref(&control), &[], &primitives, &device, None, 256, 256, Some(&control.id), None)
                .expect("control chain builds");
        apply(&mut cg0, &control.params);
        let slot0 = &cg0.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg0, slot0),
            ParamValue::Float(0.3),
            "with an identity binding, the stock zoom slot value passes straight through",
        );

        // With a ×2 reshape on the `zoom` binding: inner sees 0.6, slot
        // still reads 0.3.
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_slot(&mut fx, "zoom", 0.3);
        fx.graph = Some(def_with_binding_scale(
            PresetTypeId::STYLIZED_FEEDBACK,
            "zoom",
            2.0,
        ));
        fx.graph_version = fx.graph_version.wrapping_add(1);
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, Some(&fx.id), None)
                .expect("reshaped chain builds");
        apply(&mut cg, &fx.params);
        let slot = &cg.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg, slot),
            ParamValue::Float(0.6),
            "a ×2 reshape must scale what the inner node sees (0.3 → 0.6)",
        );
        // The invariant: the value slot the modulation surface writes is
        // byte-identical with and without the reshape.
        assert_eq!(
            fx.params.get("zoom").unwrap().value,
            0.3,
            "the reshape must NEVER rewrite the value slot — that slot \
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
            value_labels: Vec::new(),
            section: None,
        });
        // Drag the user-tail slider to `translate_value`. With static
        // count = 3 (amount, zoom, rotate) the user binding is the 4th
        // manifest entry, keyed by its binding id.
        assert_eq!(
            fx.params.len(),
            4,
            "StylizedFeedback with 3 static + 1 user-tail = 4 param slots",
        );
        set_slot(&mut fx, "user.affine.translate_x.1", translate_value);
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

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("StylizedFeedback chain with one user binding builds");

        let slot = cg
            .effect_nodes
            .first()
            .expect("StylizedFeedback contributes one effect slot");
        // `EffectSlot` no longer stores a static count; the static prefix is
        // the run of `BindingSource::Static` entries at the head of the
        // unified bindings list.
        let n_static = slot
            .bound
            .bindings
            .iter()
            .filter(|b| matches!(b.source, crate::node_graph::BindingSource::Static))
            .count();
        assert_eq!(
            slot.bound.bindings.len(),
            n_static + 1,
            "user-tail binding for affine.translate_x must hydrate at build time",
        );
        let user_rb = &slot.bound.bindings[n_static];
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

        let mut cg = PresetRuntime::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
            None,
        )
        .expect("StylizedFeedback chain with one user binding builds");

        // Mirror the per-frame apply that `run()` would execute:
        // walk the slot's unified bindings against fx.params.
        let slot = &mut cg.effect_nodes[0];
        slot.bound.apply(&mut cg.graph, &fx.params);

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
            value_labels: Vec::new(),
            section: None,
        });
        // Leave the outer slot at its declared default so the test
        // depends on the seed pass, not on the apply-with-divergent-
        // value path.
        assert_eq!(fx.params.len(), 4);
        set_slot(&mut fx, "user.affine.translate_x.1", 0.42);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
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
mod bug080_manifest_gate_tests {
    //! PARAM_MANIFEST_GATE_DESIGN.md P1, INV-1: a provisional manifest
    //! (built against an incomplete registry, `pending_wire` still `Some`)
    //! must never reach `PresetRuntime::try_build` silently.
    use manifold_core::effects::PresetInstance;

    /// A bare `PresetInstance` deserialize referencing an effect type that
    /// isn't registered anywhere, with a params map — the keep-don't-drop
    /// path (BUG-036) seeds a placeholder-spec param and leaves
    /// `pending_wire` `Some` because the template never resolved. No
    /// `Project`/loader machinery needed: this is the direct, minimal
    /// repro for "manifest built provisionally, reconcile never ran".
    fn provisional_instance() -> PresetInstance {
        let json = r#"{
            "id": "bug080_test_instance",
            "effectType": "Bug080UnregisteredType",
            "params": { "foo": { "value": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).expect("deserialize test fixture");
        assert!(
            fx.manifest_provisional(),
            "fixture must be provisional (unregistered effect type, wire stash present)"
        );
        fx
    }

    #[test]
    fn bug080_provisional_manifest_asserts_at_chain_build() {
        let fx = provisional_instance();
        let result = std::panic::catch_unwind(|| super::assert_manifest_gate(&fx));
        assert!(
            result.is_err(),
            "assert_manifest_gate must panic (via debug_assert!) when handed a \
             provisional manifest — a load/ingest path skipped reconcile_param_manifests()"
        );
    }

    #[test]
    fn bug080_loader_path_never_provisional() {
        // A freshly-constructed, template-resolved instance (the shape every
        // instance is in once `PresetInstance::reconcile_manifest` — and thus
        // the loader — has actually run against a known template) must never
        // trip the gate.
        let fx = manifold_core::preset_definition_registry::create_default(
            &manifold_core::PresetTypeId::COLOR_GRADE,
        );
        assert!(
            !fx.manifest_provisional(),
            "a template-resolved instance must never be provisional"
        );
        // Must not panic.
        super::assert_manifest_gate(&fx);
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
        let assignment = assign_texture2d_slots(&plan, src_res, (64, 64));

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

#[cfg(all(test, feature = "gpu-proofs"))]
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
    //! 2. **Per-frame push**: [`PresetRuntime::run`] writes the
    //!    [`PresetContext`]'s `time` / `beat` / `aspect` / output dims
    //!    into the generator_input node's params via `set_param`.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
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

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
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

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
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

    /// Per-frame contract: after `PresetRuntime::run`, the generator_input
    /// node's `time` / `beat` / `aspect` / `output_width` /
    /// `output_height` params reflect the [`PresetContext`].
    /// Exercises the param-write half of the system; the
    /// scalar-wire-propagation half is covered by the
    /// `generator_input_params_drive_scalar_outputs` test in
    /// `boundary_nodes.rs`.
    #[test]
    fn run_pushes_frame_context_into_generator_input_params() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
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

    /// §8 D5 (2026-07-07): `trigger_count` used to stay pinned at 0.0 for
    /// effect-chain generator_input nodes ("clip-side concepts that don't
    /// reach the effect chain"). This is the effect-chain half of the P2
    /// gate — the generator half lives in
    /// `generator_renderer::tests` (`effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode`).
    /// Together they prove the SAME effective count (clip edge + audio
    /// fires) reaches both a generator's own graph and an effect chain on
    /// the same layer.
    #[test]
    fn run_feeds_nonzero_trigger_count_into_generator_input_effect_slot() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );
        let mut native_enc = device.create_encoder("generator-input-trigger-count-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        // A layer whose generator has been triggered 7 times (clip launches
        // + audio fires, already summed by the caller per §8 D1) — the
        // effect chain on that same layer must see the SAME 7, not the old
        // pinned 0.0.
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
            trigger_count: 7,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let trigger_count = node.params.get("trigger_count").and_then(|v| match v {
            ParamValue::Float(f) => Some(*f),
            _ => None,
        });
        assert_eq!(
            trigger_count,
            Some(7.0),
            "effect chain's generator_input.trigger_count must reflect the \
             owning layer's effective count (D5), not stay pinned at 0.0"
        );
    }

    /// §8 D6 — Strobe reachability proof: the bundled Strobe preset's
    /// `clip_trigger` card (Trigger Gate → Envelope Decay → Max-combine with
    /// the beat gate) actually flashes when the layer's effective
    /// `trigger_count` jumps, and does NOT when the card is off. This is the
    /// concrete "kick fires Strobe" acceptance demo at the L1 (graph-value)
    /// level — the live app/stem look is still L4-owed (logged in the design
    /// doc), but this proves the wiring is live, not just present in the JSON.
    #[test]
    fn strobe_clip_trigger_card_flashes_on_trigger_count_jump_when_enabled() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let run_and_read_flash_amount = |clip_trigger_on: bool| -> f32 {
            let mut fx = manifold_core::preset_definition_registry::create_default(
                &PresetTypeId::new("Strobe"),
            );
            if let Some(p) = fx.params.get_mut("clip_trigger") {
                p.value = if clip_trigger_on { 1.0 } else { 0.0 };
                p.base = p.value;
            } else {
                panic!("Strobe must ship a clip_trigger card (§8 D6)");
            }

            let mut cg = PresetRuntime::try_build(
                std::slice::from_ref(&fx),
                &[],
                &primitives,
                &device,
                None,
                64,
                64,
                None,
                None,
            )
            .expect("Strobe chain builds");

            let input = crate::render_target::RenderTarget::new(
                &device,
                64,
                64,
                GpuTextureFormat::Rgba16Float,
                "strobe-test-input",
            );
            let mut native_enc = device.create_encoder("strobe-trigger-test");
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);

            let ctx_at = |trigger_count: u32| PresetContext {
                time: 0.0,
                // beat = 0.0 parks node.beat_gate's square wave at 0 (phase
                // 0.0 < duty 0.5) so the Max-combine isolates the trigger
                // path — a bare beat-gate contribution would confound the
                // assertion below.
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: 64,
                height: 64,
                output_width: 64,
                output_height: 64,
                aspect: 1.0,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count,
            };

            // Watch combine_gate's scalar I/O — `preview_scalar_io` only
            // captures for a NON-texture-outputting node (`node.math`'s `out`
            // is a bare scalar, unlike `flash`'s image output, which the
            // executor deliberately skips scalar capture for — see
            // `execution.rs`'s preview-capture step: image nodes show their
            // texture, not numbers). `.params` was tried first and rejected:
            // it only reflects bound/set values, never what a port-shadowed
            // wire evaluates to (confirmed by inspection — combine_gate's and
            // flash's `.params` stayed at their authoring defaults across both
            // frames below, even though the wires clearly carried real data).
            cg.set_preview_target(&fx.id, Some(&manifold_core::NodeId::new("combine_gate")));

            // Frame 1: baseline at trigger_count 0, settles initial state.
            cg.run(&mut gpu, &input.texture, &[fx.clone()], &[], &ctx_at(0));
            // Frame 2: the layer's effective count jumps (a kick fired).
            cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx_at(5));

            let (_inputs, outputs) = cg.preview_scalar_io();
            outputs
                .iter()
                .find(|(name, _)| name == "out")
                .map(|(_, v)| *v)
                .expect("combine_gate's watched scalar outputs must include `out`")
        };

        let on = run_and_read_flash_amount(true);
        let off = run_and_read_flash_amount(false);

        // node.envelope_decay snaps to 1.0 THEN decays once by this frame's dt
        // in the same evaluate() call, so the observable post-frame value
        // after a fire is exp(-decay_rate * dt) = exp(-12/60) ≈ 0.819, never
        // a full 1.0 — 0.7 comfortably separates "just fired" from "at rest".
        assert!(
            on > 0.7,
            "clip_trigger ON: a trigger_count jump must snap the envelope \
             (and therefore flash.amount, via the Max-combine) toward 1.0 \
             (observably ~0.82 one frame later), got {on}"
        );
        assert!(
            off < 0.1,
            "clip_trigger OFF: the Trigger Gate must absorb the count jump \
             so flash.amount stays at the beat gate's (parked-at-0) value, got {off}"
        );
    }

    /// **The production main-path proof (design §12.3 step 5).** With the freeze
    /// toggle on (default), [`PresetRuntime::try_build`] renders a canonical
    /// ColorGrade card through the FUSED node, not the 7 atoms: the built chain
    /// graph contains one `node.wgsl_compute` and none of the original
    /// `node.exposure` / `node.mix` workers, and it runs one frame producing an
    /// output texture. This is what puts the optimised fused kernel on screen.
    #[test]
    fn colorgrade_chain_renders_via_fused_node() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        // Honor the kill-switch: when MANIFOLD_FREEZE is off this path is
        // intentionally the unfused one, so the assertion wouldn't hold.
        if !crate::node_graph::freeze::install::freeze_enabled() {
            return;
        }

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::new("ColorGrade"));

        let mut cg = PresetRuntime::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
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
            !type_ids.contains(&"node.exposure") && !type_ids.contains(&"node.mix"),
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_error_tests {
    //! The chain runner accumulates structured errors during build
    //! and per-frame run. Each entry carries the effect's identity
    //! so a future editor surface can attach it to the right card.
    //!
    //! Today the immediate user-visible benefit is the consistent
    //! `[chain-error]` terminal log; tomorrow these are the data
    //! the editor reads via [`PresetRuntime::errors`]. The tests below
    //! pin one variant from the per-build path so the surface
    //! doesn't silently regress.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{PresetInstance, ParamConvert, UserParamBinding};

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
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
            value_labels: Vec::new(),
            section: None,
        });

        let cg = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
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

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("clean Invert chain builds");

        assert!(
            cg.errors().is_empty(),
            "clean chain must have no structured errors; got {:?}",
            cg.errors()
        );
    }
}

#[cfg(test)]
mod generator_runtime_tests {
    //! Generator construction + per-frame regression tests (folded in from the
    //! deleted `JsonGraphGenerator` module). They drive the `from_*` generator
    //! constructors and the `render`/`apply_param_values`/`resize`/preview
    //! surface of the unified [`PresetRuntime`].
    use super::*;
    use crate::node_graph::PrimitiveRegistry;
    use manifold_core::Beats;
    use manifold_core::Seconds;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::params::Param;

    /// Build a single id-keyed manifest param for test [`ParamManifest`]
    /// literals — the id-keyed replacement for the old positional `&[f32]`
    /// slice `apply_param_values` used to take.
    fn slot(id: &str, value: f32) -> Param {
        let mut p = Param::bundled(ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        });
        p.value = value;
        p.base = value;
        p.exposed = true;
        p
    }

    /// Build a [`ParamManifest`] from `(id, value)` pairs, in the order
    /// given — mirrors the positional `&[f32]` slices these tests used to
    /// pass to `apply_param_values` before the id-keyed manifest replaced it.
    fn manifest(pairs: &[(&str, f32)]) -> ParamManifest {
        ParamManifest::from_params(pairs.iter().map(|(id, v)| slot(id, *v)).collect())
    }

    /// Regression for the "Lissajous repeats back-to-back in clip-trigger mode"
    /// bug: two bindings keyed by the same outer-card id (`clip_trigger`) must
    /// both pick up that slider's value (fan-out by source id, not position).
    #[test]
    fn fan_out_binding_writes_every_target_with_the_same_outer_value() {
        let json = include_str!("../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");

        // Address inner nodes by stable node_id (grouping prefixes handles,
        // node_id survives the flatten the loader applies).
        let mux_x_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("mux_x"))
            .expect("Lissajous declares a `mux_x` node");
        let mux_y_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("mux_y"))
            .expect("Lissajous declares a `mux_y` node");

        g.apply_param_values(&manifest(&[
            ("freq_x_rate", 0.13),
            ("freq_y_rate", 0.09),
            ("phase_rate", 0.07),
            ("line", 0.002),
            ("show_verts", 1.0),
            ("vert_size", 1.0),
            ("animate", 0.0),
            ("speed", 1.0),
            ("window", 0.1),
            ("scale", 1.0),
            ("clip_trigger", 1.0),
        ]));

        let mux_x = g.graph.get_node(mux_x_id).unwrap();
        assert!(
            matches!(
                mux_x.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_x.selector should be 1.0, got {:?}",
            mux_x.params.get("selector"),
        );
        let mux_y = g.graph.get_node(mux_y_id).unwrap();
        assert!(
            matches!(
                mux_y.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_y.selector should be 1.0 (fan-out from same `clip_trigger` outer \
             slider as mux_x), got {:?}",
            mux_y.params.get("selector"),
        );
    }

    /// BUG-104 — `clear_trigger_state` on a REAL shipped preset (Lissajous)
    /// walks the graph, finds exactly the nodes `is_trigger_latch` flags
    /// (`ratio` — `node.frequency_ratio`), and purges ONLY their
    /// `StateStore` buckets, leaving an ordinary node's (`render` —
    /// `node.draw_lines`) bucket untouched. No GPU needed —
    /// `clear_trigger_state` never touches the backend.
    #[test]
    fn clear_trigger_state_purges_only_flagged_nodes_state_store_buckets() {
        use crate::node_graph::NodeState;

        struct Probe;
        impl NodeState for Probe {}

        let json = include_str!("../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");

        let ratio_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("ratio"))
            .expect("Lissajous declares a `ratio` (frequency_ratio) node");
        let render_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("render"))
            .expect("Lissajous declares a `render` (draw_lines) node");

        assert!(
            g.graph.get_node(ratio_id).unwrap().node.is_trigger_latch(),
            "frequency_ratio must flag itself as a trigger latch"
        );
        assert!(
            !g.graph.get_node(render_id).unwrap().node.is_trigger_latch(),
            "draw_lines is not a trigger latch — clear_trigger_state must leave it alone"
        );

        // Seed a StateStore bucket under BOTH node ids (owner_key 0, the
        // generator convention) — clear_trigger_state must purge only the
        // one belonging to the flagged node.
        g.state_store.insert(ratio_id, 0, Probe);
        g.state_store.insert(render_id, 0, Probe);

        g.clear_trigger_state();

        assert!(
            g.state_store.get::<Probe>(ratio_id, 0).is_none(),
            "trigger-latch node's StateStore bucket must be purged"
        );
        assert!(
            g.state_store.get::<Probe>(render_id, 0).is_some(),
            "non-latch node's StateStore bucket must survive a trigger-only clear"
        );
    }

    /// BUG-104 Part 5(b) — the live build-time counterpart to
    /// `trigger_shadow_class_guard.rs`'s offline sweep: the REAL shipped
    /// Lissajous.json (post BUG-104 Part 3 fix) must build with ZERO
    /// `TriggerShadowsContinuousBinding` errors — proving the fix is
    /// structurally clean, not just visually plausible.
    #[test]
    fn lissajous_builds_with_no_trigger_shadow_errors() {
        let json = include_str!("../assets/generator-presets/Lissajous.json");
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");
        let shadow_errors: Vec<_> = g
            .errors()
            .iter()
            .filter(|e| matches!(e, ChainError::TriggerShadowsContinuousBinding { .. }))
            .collect();
        assert!(
            shadow_errors.is_empty(),
            "Lissajous must build with no BUG-104 trigger-shadow errors, got {shadow_errors:?}"
        );
    }

    /// BUG-104 Part 5(b) — same synthetic pre-fix shape as
    /// `trigger_shadow_class_guard.rs`'s regression test, but exercised
    /// through the REAL build path (`PresetRuntime::from_def` via
    /// `from_json_str`) to prove the warning reaches `PresetRuntime::
    /// errors()` — the channel editor UI / MCP-driven mutations / agent-
    /// authored graphs all read, not just the offline sweep test.
    #[test]
    fn from_json_str_surfaces_trigger_shadow_as_a_chain_error() {
        let json = r#"{
            "version": 2,
            "name": "SyntheticPreFixShape",
            "nodes": [
                { "id": 0, "nodeId": "input", "typeId": "system.generator_input" },
                { "id": 1, "nodeId": "lfo_x", "typeId": "node.lfo",
                  "params": { "angular_rate": { "type": "Float", "value": 0.13 } } },
                { "id": 2, "nodeId": "mux_x", "typeId": "node.switch_value" },
                { "id": 3, "nodeId": "uv", "typeId": "node.uv_field" },
                { "id": 4, "nodeId": "final_output", "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in_0" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ],
            "presetMetadata": {
                "id": "SyntheticPreFixShape",
                "displayName": "Synthetic",
                "category": "Geometry",
                "oscPrefix": "synthetic",
                "params": [
                    { "id": "freq_x_rate", "name": "Freq X Rate", "min": 0.0, "max": 1.0,
                      "defaultValue": 0.13, "wholeNumbers": false, "isToggle": false, "isTrigger": false },
                    { "id": "clip_trigger", "name": "Clip Trigger", "min": 0.0, "max": 1.0,
                      "defaultValue": 0.0, "wholeNumbers": false, "isToggle": true, "isTriggerGate": true, "isTrigger": false }
                ],
                "bindings": [
                    { "id": "freq_x_rate", "label": "Freq X Rate", "defaultValue": 0.13,
                      "target": { "kind": "node", "nodeId": "lfo_x", "param": "angular_rate" },
                      "convert": { "type": "Float" } },
                    { "id": "clip_trigger", "label": "Clip Trigger", "defaultValue": 0.0,
                      "target": { "kind": "node", "nodeId": "mux_x", "param": "selector" },
                      "convert": { "type": "Float" } }
                ]
            }
        }"#;
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("synthetic pre-fix-shaped preset must still build (this is a warning, not a \
                     hard failure — the graph runs, it just has a shadowed fader)");
        let shadow_errors: Vec<_> = g
            .errors()
            .iter()
            .filter(|e| matches!(e, ChainError::TriggerShadowsContinuousBinding { .. }))
            .collect();
        assert_eq!(
            shadow_errors.len(),
            1,
            "from_json_str (-> from_def) must surface the trigger-shadow finding through \
             PresetRuntime::errors(), got {shadow_errors:?}"
        );
    }

    /// Regression for the "Plasma looks frozen" bug: outer-card slider values
    /// must reach the inner-node param via the preset's declared bindings.
    #[test]
    fn apply_param_values_routes_into_inner_node_params() {
        let json = include_str!("../assets/generator-presets/Plasma.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Plasma preset must load");
        let plasma_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "plasma")
            .map(|(_, id)| id)
            .expect("Plasma preset declares a node with handle `plasma`");

        g.apply_param_values(&manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]));

        let inst = g.graph.get_node(plasma_id).unwrap();
        assert!(matches!(
            inst.params.get("complexity"),
            Some(ParamValue::Float(v)) if (*v - 0.75).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("contrast"),
            Some(ParamValue::Float(v)) if (*v - 0.42).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("speed"),
            Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("scale"),
            Some(ParamValue::Float(v)) if (*v - 1.5).abs() < 1e-5
        ));
    }

    /// BUG-182 regression: a String param set directly on a node (the graph
    /// editor's param edit / file picker writes NODE params, not the card's
    /// `clip.string_params` map) must survive host string-param pushes whose
    /// map lacks the binding's key. The pre-fix behavior fell back to the
    /// binding's declared default for absent keys, so the card's empty
    /// `hdri_file` binding overwrote `node.hdri_source`'s `path` every frame.
    #[test]
    fn string_params_absent_key_does_not_clobber_node_level_value() {
        let json = include_str!("../assets/generator-presets/Text.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Text preset must load");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("Text preset declares a node with handle `render_text`");

        // Construction seed: the def node carries no `text` param, so the
        // binding's declared default is planted.
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HELLO"
        ));

        // Direct node-level write — what SetGraphNodeParamCommand +
        // apply_inner_param_overrides produce for a graph-editor edit.
        g.graph
            .set_param(
                render_text,
                "text",
                ParamValue::String(std::sync::Arc::new("DIRECT".to_string())),
            )
            .expect("render_text declares `text`");

        // Neither a missing host map nor a map lacking the key may touch it.
        g.apply_string_params(None);
        let only_font: std::collections::BTreeMap<String, String> =
            [("fontFamily".to_string(), "Menlo".to_string())].into_iter().collect();
        g.apply_string_params(Some(&only_font));
        assert!(
            matches!(
                g.graph.get_node(render_text).unwrap().params.get("text"),
                Some(ParamValue::String(s)) if s.as_str() == "DIRECT"
            ),
            "absent host key must leave the node-level value alone"
        );
        // A present key in the same map DID write (only absent keys skip).
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("fontFamily"),
            Some(ParamValue::String(s)) if s.as_str() == "Menlo"
        ));
    }

    /// The other half of BUG-182: an explicit host value must still win, land
    /// live, and not be reverted by later pushes that omit the key.
    #[test]
    fn string_params_explicit_host_value_wins_and_sticks() {
        let json = include_str!("../assets/generator-presets/Text.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Text preset must load");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("render_text handle");

        let host: std::collections::BTreeMap<String, String> =
            [("text".to_string(), "HOST".to_string())].into_iter().collect();
        g.apply_string_params(Some(&host));
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HOST"
        ));

        // A later push that omits the key leaves the host's value live
        // (sticky — defaults are a construction-time seed, not a per-frame
        // re-assertion).
        g.apply_string_params(None);
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HOST"
        ));
    }

    /// Construction seeding precedence (BUG-182): when the def node carries
    /// its OWN value for a string-bound param (a def-baked file path set
    /// directly on the node), that value must survive construction — the
    /// binding's declared default is only a fallback for params the def
    /// leaves unset.
    #[test]
    fn string_binding_construction_seed_respects_def_node_param_over_default() {
        use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
        let json = include_str!("../assets/generator-presets/Text.json");
        let mut def: EffectGraphDef =
            serde_json::from_str(json).expect("Text preset JSON must parse");
        let node_doc = def
            .nodes
            .iter_mut()
            .find(|n| n.node_id.as_str() == "render_text")
            .expect("render_text node doc");
        node_doc.params.insert(
            "text".to_string(),
            SerializedParamValue::String {
                value: "FROM_DEF".to_string(),
            },
        );

        let g = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None)
            .expect("Text preset with a def-baked `text` param must build");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("render_text handle");

        assert!(
            matches!(
                g.graph.get_node(render_text).unwrap().params.get("text"),
                Some(ParamValue::String(s)) if s.as_str() == "FROM_DEF"
            ),
            "def node param must win over the binding's declared default (\"HELLO\")"
        );
        // A param the def does NOT set still gets the binding default.
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("fontFamily"),
            Some(ParamValue::String(s)) if s.is_empty()
        ));
    }

    /// Regression for the OilyFluid "Speed slider snaps back" bug.
    /// `apply_inner_param_overrides` must clear the binding cache so the next
    /// `apply_param_values` re-asserts the bound card value over the def default.
    #[test]
    fn inner_param_overrides_re_assert_bound_card_values() {
        use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
        let json = include_str!("../assets/generator-presets/Plasma.json");
        let registry = PrimitiveRegistry::with_builtin();
        let mut g = PresetRuntime::from_json_str(json, &registry).expect("Plasma preset must load");
        let plasma_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "plasma")
            .map(|(_, id)| id)
            .expect("plasma handle");

        let card_values = manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]);
        g.apply_param_values(&card_values);
        assert!(matches!(
            g.graph.get_node(plasma_id).unwrap().params.get("speed"),
            Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
        ));

        let mut def: EffectGraphDef = serde_json::from_str(json).unwrap();
        for node in &mut def.nodes {
            if node.handle.as_deref() == Some("plasma") {
                node.params
                    .insert("speed".to_string(), SerializedParamValue::Float { value: 9.0 });
            }
        }
        g.apply_inner_param_overrides(&def);

        g.apply_param_values(&card_values);
        assert!(
            matches!(
                g.graph.get_node(plasma_id).unwrap().params.get("speed"),
                Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
            ),
            "bound Speed must re-assert its card value (2.5) over the def's baked 9.0; got {:?}",
            g.graph.get_node(plasma_id).unwrap().params.get("speed"),
        );
    }

    /// Generator mirror of the effect reshape proof: a `scale` on the card
    /// binding's `BindingDef` reshapes what the inner node sees.
    #[test]
    fn stock_generator_reshape_changes_inner_node() {
        let json = include_str!("../assets/generator-presets/Plasma.json");
        let registry = PrimitiveRegistry::with_builtin();

        let plasma_id = |g: &PresetRuntime| {
            g.graph
                .handles()
                .find(|(h, _)| *h == "plasma")
                .map(|(_, id)| id)
                .expect("plasma handle")
        };
        let values = manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]);

        let mut g0 = PresetRuntime::from_json_str(json, &registry).expect("load");
        g0.apply_param_values(&values);
        let id0 = plasma_id(&g0);
        assert!(matches!(
            g0.graph.get_node(id0).unwrap().params.get("complexity"),
            Some(ParamValue::Float(v)) if (*v - 0.75).abs() < 1e-5
        ));

        let mut def: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_str(json).expect("parse Plasma def");
        let meta = def
            .preset_metadata
            .as_mut()
            .expect("Plasma carries presetMetadata");
        meta.bindings
            .iter_mut()
            .find(|b| b.id == "complexity")
            .expect("complexity binding exists")
            .scale = 2.0;
        let reshaped_json = serde_json::to_string(&def).expect("serialize reshaped def");
        let mut g = PresetRuntime::from_json_str(&reshaped_json, &registry).expect("load");
        g.apply_param_values(&values);
        let id = plasma_id(&g);
        assert!(
            matches!(
                g.graph.get_node(id).unwrap().params.get("complexity"),
                Some(ParamValue::Float(v)) if (*v - 1.5).abs() < 1e-5
            ),
            "a ×2 reshape must scale plasma.complexity 0.75 -> 1.5, got {:?}",
            g.graph.get_node(id).unwrap().params.get("complexity"),
        );
        assert_eq!(
            values.get("complexity").unwrap().value,
            0.75,
            "the host manifest is never mutated"
        );
    }

    /// Regression for the on-stage FluidSim2D Curl bug: a binding's `scale`
    /// must fold into the inner-node param on the generator path.
    #[test]
    fn generator_binding_scale_folds_into_inner_param() {
        let json = r#"{
            "version": 1,
            "name": "ScaledBindingTest",
            "presetMetadata": {
                "id": "ScaledBindingTest",
                "displayName": "Scaled Binding Test",
                "category": "Generator",
                "oscPrefix": "scaledBindingTest",
                "params": [
                    { "id": "amt", "name": "Amount", "min": 0.0, "max": 10.0, "defaultValue": 0.0 }
                ],
                "bindings": [
                    { "id": "amt", "label": "Amount", "defaultValue": 0.0,
                      "target": { "kind": "handleNode", "handle": "so", "param": "offset" },
                      "scale": 0.5,
                      "convert": { "type": "Float" } }
                ]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "node.scale_offset_image", "handle": "so" },
                { "id": 3, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;

        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("scaled-binding test preset must load");
        let so_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "so")
            .map(|(_, id)| id)
            .expect("preset declares a `so` handle");

        g.apply_param_values(&manifest(&[("amt", 4.0)]));

        let inst = g.graph.get_node(so_id).unwrap();
        assert!(
            matches!(
                inst.params.get("offset"),
                Some(ParamValue::Float(v)) if (*v - 2.0).abs() < 1e-5
            ),
            "generator binding scale dropped: offset should be 4.0 * 0.5 = 2.0, got {:?}",
            inst.params.get("offset"),
        );
    }

    /// BUG-078 regression (fixed). Post-PARAM_STORAGE_BOUNDARIES-P2 (D4/D12),
    /// a calibration writes ONLY `PresetInstance.params[id].spec` — the graph's
    /// `preset_metadata.params` shadow is left stale until save (D12 derives
    /// it at serialize time, not before). A structural graph edit rebuilds
    /// the generator's `PresetRuntime` through EXACTLY this constructor
    /// (`registry.create_with_override` -> `PresetRuntime::from_def_with_device`;
    /// `from_def` here is the mock-backend equivalent).
    ///
    /// The fix threads the live per-instance `ParamManifest` into `from_def`,
    /// which overlays each param's reshape (range/curve/invert) from the
    /// manifest `spec` over the graph's shadow — so a post-calibration rebuild
    /// honors the fresh range. This test passes `Some(&values)` (the fresh
    /// manifest) and asserts the reshape follows it, not the stale shadow.
    ///
    /// The manifest built below stands in for what `EditParamMappingCommand`
    /// (`manifold-editing/src/commands/effects.rs`, `apply_to_manifest_spec`)
    /// actually writes into `PresetInstance.params["amt"].spec` on a real
    /// calibration: only `max` widens, 1.0 -> 2.0, curve stays Exponential so
    /// the note actually engages (`apply_card_reshape` only consults min/max
    /// when `invert || curve != Linear` — a min/max-only edit on an
    /// otherwise-identity binding can't be observed this way).
    #[test]
    fn generator_rebuild_reshape_honors_live_manifest_over_stale_shadow() {
        let json = r#"{
            "version": 1,
            "name": "StaleReshapeTest",
            "presetMetadata": {
                "id": "StaleReshapeTest",
                "displayName": "Stale Reshape Test",
                "category": "Generator",
                "oscPrefix": "staleReshapeTest",
                "params": [
                    { "id": "amt", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 0.0 }
                ],
                "bindings": [
                    { "id": "amt", "label": "Amount", "defaultValue": 0.0,
                      "target": { "kind": "handleNode", "handle": "so", "param": "offset" },
                      "convert": { "type": "Float" } }
                ]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "node.scale_offset_image", "handle": "so" },
                { "id": 3, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;

        // The def exactly as it sits in memory right after a calibration:
        // P2 writes ONLY the manifest, so this shadow still carries the
        // ORIGINAL (pre-calibration) range — with the curve engaged so
        // min/max actually enters the transform.
        let mut def: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_str(json).expect("parse StaleReshapeTest def");
        {
            let meta = def
                .preset_metadata
                .as_mut()
                .expect("StaleReshapeTest carries presetMetadata");
            let p = meta
                .params
                .iter_mut()
                .find(|p| p.id == "amt")
                .expect("amt param spec");
            p.curve = manifold_core::macro_bank::MacroCurve::Exponential;
            p.min = 0.0;
            p.max = 1.0; // STALE — pre-calibration range
        }

        // The freshly-calibrated manifest a rebuild SHOULD honor: same
        // curve, widened range 0..2 — exactly what `EditParamMappingCommand`
        // would have just written into `PresetInstance.params["amt"].spec`.
        let mut values = manifest(&[("amt", 1.0)]);
        {
            let p = values.get_mut("amt").expect("amt manifest entry");
            p.spec.curve = manifold_core::macro_bank::MacroCurve::Exponential;
            p.spec.min = 0.0;
            p.spec.max = 2.0; // FRESH — post-calibration
        }

        // This IS the production rebuild path (mock-backend form of
        // `PresetRuntime::from_def_with_device`). The fix threads the live
        // manifest as the reshape authority; the generator_renderer rebuild
        // path passes `layer.gen_params().params` here.
        let mut g = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), Some(&values))
            .expect("StaleReshapeTest def loads");
        g.apply_param_values(&values);

        let so_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "so")
            .map(|(_, id)| id)
            .expect("preset declares a `so` handle");
        let offset = match g.graph.get_node(so_id).unwrap().params.get("offset") {
            Some(ParamValue::Float(v)) => *v,
            other => panic!("expected float, got {other:?}"),
        };

        // Post-fix behavior: amt=1.0 normalized against the FRESH 0..2 range
        // is 0.5 -> curved (Exponential, n^2) to 0.25 -> re-scaled to 0..2 ->
        // 0.5. The pre-fix (stale-shadow) output was 1.0 (normalized against
        // the STALE 0..1 range: 1.0 clamped to n=1.0, curved to 1.0, no
        // reshape at all). 0.5 proves the manifest's widened range won.
        assert!(
            (offset - 0.5).abs() < 1e-5,
            "a structural rebuild must resolve `amt`'s reshape from the live \
             manifest spec (min=0,max=2), not the graph's stale \
             `preset_metadata.params` shadow (min=0,max=1) — got {offset} \
             (1.0 would be the STALE 0..1 range's output)",
        );
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn trivial_passthrough_generator_loads_and_executes() {
        let json = r#"{
            "version": 1,
            "name": "TestPassthrough",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;

        let mut preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("trivial generator preset must load");
        assert_eq!(preset.type_id().as_str(), "TestPassthrough");
        preset.set_frame_context(1.5, 0.5, 1.78, 4.0, 0.25, 1920.0, 1080.0);
        preset.execute_frame(frame_time());
    }

    /// BUG per PARAM_TWO_WAY_BINDING_DESIGN.md P2 D5: a wired scalar input
    /// is resolved live, per-frame, via `EffectNodeContext::scalar_or_param`
    /// (wire first, param second) — it never writes back into
    /// `NodeInstance::params`. The old `live_node_params` read only the
    /// param map, so the editor's value inspector froze on a wire-driven
    /// scalar param while the render kept moving. `node.value` (a constant
    /// control source, `pure: true`) wired into
    /// `node.scale_offset_image`'s `scale` port — whose own `scale` param
    /// defaults to `1.0` and is never wired-through — is the minimal
    /// control-wire fixture that reproduces it.
    #[test]
    fn live_node_params_reports_wire_value_not_stale_param_default() {
        let json = r#"{
            "version": 1,
            "name": "TestWireTap",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "nodeId": "uv", "handle": "uv" },
                { "id": 2, "typeId": "node.value", "nodeId": "src", "handle": "src",
                  "params": { "value": { "type": "Float", "value": 0.75 } } },
                { "id": 3, "typeId": "node.scale_offset_image", "nodeId": "scaler", "handle": "scaler" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "scale" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("wire-tap fixture must load");
        let scaler_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("scaler"))
            .expect("fixture declares a `scaler` node");

        g.execute_frame(frame_time());

        // Sanity: the wire never writes NodeInstance::params — the param
        // map is still the primitive's declared default.
        assert!(
            matches!(
                g.graph.get_node(scaler_id).unwrap().params.get("scale"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-6
            ),
            "sanity: a wired scalar input must not write NodeInstance::params, got {:?}",
            g.graph.get_node(scaler_id).unwrap().params.get("scale"),
        );

        // The live tap must report the WIRE's value (0.75 from `src`), not
        // the stale param-map default (1.0).
        let scaler_node_id = g.graph.get_node(scaler_id).unwrap().node_id.clone();
        let live = g.live_node_params_watched();
        let scaler_values = live
            .iter()
            .find(|(id, _)| *id == scaler_node_id)
            .map(|(_, values)| values)
            .expect("scaler node reports live params");
        let scale_v = *scaler_values
            .iter()
            .find(|(name, _)| *name == "scale")
            .map(|(_, v)| v)
            .expect("scale is a declared param");
        assert!(
            (scale_v - 0.75).abs() < 1e-5,
            "live_node_params_watched should report the wire's live value \
             (0.75), not the stale param-map default (1.0); got {scale_v}"
        );
    }

    /// `PresetRuntime` holds a `Graph` which doesn't impl Debug, so we
    /// destructure the Result by hand rather than `expect_err`.
    fn unwrap_err(
        r: Result<PresetRuntime, JsonGeneratorLoadError>,
    ) -> JsonGeneratorLoadError {
        match r {
            Ok(_) => panic!("expected JsonGeneratorLoadError, got Ok(PresetRuntime)"),
            Err(e) => e,
        }
    }

    #[test]
    fn missing_generator_input_is_a_clean_error() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [ { "id": 0, "typeId": "system.final_output" } ],
            "wires": []
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingGeneratorInput),
            "got {err:?}"
        );
    }

    #[test]
    fn infra_session_integration_smoke_test() {
        let json = r#"{
            "version": 2,
            "name": "InfraSmoke",
            "presetMetadata": {
                "id": "InfraSmoke",
                "displayName": "Infra Smoke",
                "category": "Diagnostic",
                "oscPrefix": "infra_smoke",
                "params": [],
                "bindings": []
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.wgsl_compute", "handle": "branch_a" },
                { "id": 2, "typeId": "node.wgsl_compute", "handle": "branch_b" },
                { "id": 3, "typeId": "node.switch_texture", "handle": "mux" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "selector" },
                { "fromNode": 1, "fromPort": "output_tex", "toNode": 3, "toPort": "in_0" },
                { "fromNode": 2, "fromPort": "output_tex", "toNode": 3, "toPort": "in_1" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("infra smoke preset must load");
        assert_eq!(preset.type_id().as_str(), "InfraSmoke");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_strange_attractor_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled StrangeAttractor must load + compile");
        assert_eq!(preset.type_id().as_str(), "StrangeAttractor");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_plasma_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/Plasma.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled Plasma must load + compile");
        assert_eq!(preset.type_id().as_str(), "Plasma");
    }

    /// **I5** (`docs/CINEMATIC_POST_DESIGN.md`): the DoF chain (camera_lens ->
    /// render_scene[depth wired] -> coc_from_depth -> variable_blur H -> V)
    /// loads and compiles as ordinary preset JSON. CinematicScene was pulled
    /// from the bundled library 2026-07-16 (3D-infra test rig, not show
    /// content) and lives in `assets/reference-presets/`; the I5 gate keeps
    /// compiling it from there so the DoF-chain build check survives the
    /// unbundling (mirrors `bundled_plasma_loads_and_compiles` above).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_cinematic_scene_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../assets/reference-presets/CinematicScene.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled CinematicScene must load + compile");
        assert_eq!(preset.type_id().as_str(), "CinematicScene");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn resize_re_pre_allocates_array_buffers() {
        use crate::node_graph::{Backend, PortType};
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("Lissajous preset must load");

        let array_resources: Vec<ResourceId> = (0..g.plan.resource_count() as u32)
            .map(ResourceId)
            .filter(|id| matches!(g.plan.resource_type(*id), Some(PortType::Array(_))))
            .collect();
        assert!(
            !array_resources.is_empty(),
            "Lissajous preset must produce at least one Array<T> wire",
        );

        {
            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("production path constructs a MetalBackend");
            for &res in &array_resources {
                let slot = metal
                    .slot_for(res)
                    .unwrap_or_else(|| panic!("Array resource {res:?} unbound after construction"));
                assert!(
                    Backend::array_buffer(metal, slot).is_some(),
                    "Array resource {res:?} has no backing buffer after construction",
                );
            }
        }

        g.resize(&device, 1280, 720);

        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");
        for &res in &array_resources {
            let slot = metal
                .slot_for(res)
                .unwrap_or_else(|| panic!("Array resource {res:?} unbound after resize"));
            assert!(
                Backend::array_buffer(metal, slot).is_some(),
                "Array resource {res:?} has no backing buffer after resize",
            );
        }
    }

    /// Live project-resolution change must not kill a particle preset
    /// (Peter's report on Cymatics, 2026-07-16: "breaks when I change
    /// project resolution"). `resize()` wipes every pinned binding
    /// including Array<T> wires; a particle sim whose state rides those
    /// buffers (or whose re-seed never re-fires) comes back dead — black
    /// output, sand gone. This renders warm-up frames, resizes, renders
    /// again, and asserts the output still carries energy.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn cymatics_survives_live_resize() {
        use crate::preset_context::PresetContext;
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/Cymatics.json");
        let registry = PrimitiveRegistry::with_builtin();
        let format = GpuTextureFormat::Rgba16Float;
        let (w0, h0) = (512u32, 512u32);
        let mut g = PresetRuntime::from_json_str_with_device(
            json, &registry, device.arc(), w0, h0, format, None,
        )
        .expect("Cymatics preset must load");

        let max_luma = |g: &mut PresetRuntime, w: u32, h: u32, frames: u32, base: u32| -> f32 {
            let target = RenderTarget::new(&device, w, h, format, "cymatics-resize-test");
            for f in 0..frames {
                let ctx = PresetContext {
                    time: (base + f) as f64 / 60.0,
                    beat: 0.0,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: i64::from(base + f),
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let mut enc = device.create_encoder("cymatics-resize-frame");
                {
                    let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut enc, &device);
                    g.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut rb = device.create_encoder("cymatics-resize-readback");
            rb.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            rb.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer mapped");
            let px: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            px.chunks(4)
                .map(|c| half::f16::from_bits(c[0]).to_f32())
                .fold(0.0f32, f32::max)
        };

        let before = max_luma(&mut g, w0, h0, 90, 0);
        assert!(
            before > 0.05,
            "Cymatics must render visible sand before resize (max luma {before})"
        );

        let (w1, h1) = (384u32, 640u32);
        g.resize(&device, w1, h1);

        let after = max_luma(&mut g, w1, h1, 90, 90);
        assert!(
            after > 0.05,
            "Cymatics must still render visible sand after a live resize \
             (max luma {after} — resize killed the particle state)"
        );
    }

    /// Same resize-survival contract for FluidSim2D — the tuned reference
    /// particle sim. Exists to prove (or refute) that the resize kill was
    /// a class bug across particle presets, not Cymatics-specific.
    ///
    /// Verdict 2026-07-16: it IS the class bug (max luma 0 after resize
    /// with the state-clear disabled) — but the b11e6511 state-clear that
    /// rescues Cymatics does NOT rescue FluidSim2D; its re-seed path never
    /// re-arms. Tracked as BUG-175 (docs/BUG_BACKLOG.md); un-ignore when
    /// fixing it — this test is the acceptance gate.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    #[ignore = "BUG-175: FluidSim2D stays black after live resize; reproducer kept as the fix's acceptance gate"]
    fn fluidsim2d_survives_live_resize() {
        use crate::preset_context::PresetContext;
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/FluidSim2D.json");
        let registry = PrimitiveRegistry::with_builtin();
        let format = GpuTextureFormat::Rgba16Float;
        let (w0, h0) = (512u32, 512u32);
        let mut g = PresetRuntime::from_json_str_with_device(
            json, &registry, device.arc(), w0, h0, format, None,
        )
        .expect("FluidSim2D preset must load");

        let max_luma = |g: &mut PresetRuntime, w: u32, h: u32, frames: u32, base: u32| -> f32 {
            let target = RenderTarget::new(&device, w, h, format, "fluid-resize-test");
            for f in 0..frames {
                let ctx = PresetContext {
                    time: (base + f) as f64 / 60.0,
                    beat: 0.0,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: i64::from(base + f),
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let mut enc = device.create_encoder("fluid-resize-frame");
                {
                    let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut enc, &device);
                    g.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut rb = device.create_encoder("fluid-resize-readback");
            rb.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            rb.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer mapped");
            let px: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            px.chunks(4)
                .map(|c| half::f16::from_bits(c[0]).to_f32())
                .fold(0.0f32, f32::max)
        };

        let before = max_luma(&mut g, w0, h0, 90, 0);
        assert!(before > 0.05, "FluidSim2D must render before resize (max luma {before})");
        g.resize(&device, 384, 640);
        let after = max_luma(&mut g, 384, 640, 90, 90);
        assert!(
            after > 0.05,
            "FluidSim2D must still render after live resize (max luma {after})"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn aliased_array_io_routes_in_and_out_to_one_physical_slot() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");
        let mut g = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("StrangeAttractor preset must load");

        let find_node = |type_id: &str| -> NodeInstanceId {
            for step in g.plan.steps() {
                let inst = g.graph.get_node(step.node).expect("step's node");
                if inst.node.type_id().as_str() == type_id {
                    return step.node;
                }
            }
            panic!("node `{type_id}` not in compiled plan");
        };
        let integrate_node = find_node("node.wgsl_compute");
        let scatter_node = find_node("node.draw_particles");

        let resource_for = |node: NodeInstanceId, port: &str, is_input: bool| -> ResourceId {
            for step in g.plan.steps() {
                if step.node == node {
                    let ports = if is_input { &step.inputs } else { &step.outputs };
                    for &(name, id) in ports {
                        if name == port {
                            return id;
                        }
                    }
                }
            }
            panic!(
                "missing {} port `{port}` on node {node:?}",
                if is_input { "input" } else { "output" }
            );
        };

        let integrate_in_res = resource_for(integrate_node, "particles", true);
        let integrate_out_res = resource_for(integrate_node, "particles", false);
        let scatter_in_res = resource_for(scatter_node, "particles", true);

        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");

        let in_slot = metal.slot_for(integrate_in_res).expect("integrate.in bound");
        let out_slot = metal.slot_for(integrate_out_res).expect("integrate.out bound");
        let scatter_slot = metal.slot_for(scatter_in_res).expect("scatter.particles bound");

        assert_eq!(in_slot, out_slot, "aliased_array_io in→out must share a slot");
        assert_eq!(
            out_slot, scatter_slot,
            "integrate.out and scatter.particles must resolve to the same slot",
        );
        assert!(
            Backend::array_buffer(metal, in_slot).is_some(),
            "the shared slot must back a real GpuBuffer",
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn canvas_sized_array_outputs_scale_buffer_with_backend_canvas_dims() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");

        let cases = [(1280u32, 720u32), (3840u32, 2160u32)];
        for (w, h) in cases {
            let mut g = PresetRuntime::from_json_str_with_device(
                json,
                &PrimitiveRegistry::with_builtin(),
                device.arc(),
                w,
                h,
                GpuTextureFormat::Rgba16Float,
                None,
            )
            .expect("preset must load");

            let scatter = (|| {
                for step in g.plan.steps() {
                    let inst = g.graph.get_node(step.node).expect("step's node");
                    if inst.node.type_id().as_str() == "node.draw_particles" {
                        return step.node;
                    }
                }
                panic!("scatter node missing");
            })();
            let accum_res = (|| {
                for step in g.plan.steps() {
                    if step.node == scatter {
                        for &(name, id) in &step.outputs {
                            if name == "accum" {
                                return id;
                            }
                        }
                    }
                }
                panic!("scatter.accum resource missing");
            })();

            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("metal backend");
            let slot = metal.slot_for(accum_res).expect("scatter.accum unbound");
            let buf = Backend::array_buffer(metal, slot).expect("no backing buffer");
            let expected = (w as u64) * (h as u64) * 4;
            assert_eq!(
                buf.size, expected,
                "scatter.accum at canvas {w}x{h} should be {expected} bytes, got {}",
                buf.size,
            );
        }
    }

    #[test]
    fn bundled_trivial_passthrough_preset_loads_and_executes() {
        let json = include_str!("../assets/generator-presets/TrivialPassthrough.json");
        let mut preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("bundled TrivialPassthrough must load");
        assert_eq!(preset.type_id().as_str(), "TrivialPassthrough");
        preset.set_frame_context(0.0, 0.0, 1.78, 0.0, 0.0, 1920.0, 1080.0);
        preset.execute_frame(frame_time());
    }

    #[test]
    fn missing_final_output_is_a_clean_error() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [ { "id": 0, "typeId": "system.generator_input" } ],
            "wires": []
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingFinalOutput),
            "got {err:?}"
        );
    }

    /// BUG-125: a generator JSON with TWO `system.final_output` nodes used to
    /// have its tracked output resolved via `.find()` over an unordered
    /// `AHashMap`, picking one nondeterministically per process and silently
    /// overwriting the loser's texture with the canvas format at render
    /// time. Rejected loudly at load instead.
    #[test]
    fn dual_final_output_is_rejected_at_load() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input" },
                { "id": 1, "typeId": "node.uv_field" },
                { "id": 2, "typeId": "system.final_output" },
                { "id": 3, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MultipleFinalOutputs { count: 2 }),
            "got {err:?}"
        );
    }

    /// Node-output preview, grouped generator: selecting the collapsed
    /// `Flow Field` group resolves to the concrete producer of its `forceField`
    /// output. The group → producer map lives on the single segment now.
    #[test]
    fn grouped_generator_preview_resolves_group_to_producer() {
        let json = include_str!("../assets/generator-presets/FluidSim2D.json");
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("FluidSim2D preset must load");

        assert!(
            g.graph
                .instance_by_node_id(&manifold_core::NodeId::new("Flow Field"))
                .is_none(),
            "group container should have no runtime instance after flattening"
        );

        let seg = g.effect_nodes.first().expect("generator has one segment");
        let (producer, port) = seg
            .group_preview_map
            .iter()
            .find(|(group, _, _)| *group == manifold_core::NodeId::new("Flow Field"))
            .map(|(_, producer, port)| (producer.clone(), port.clone()))
            .expect("Flow Field group must be in the preview map");
        assert_eq!(
            producer,
            manifold_core::NodeId::new("field_blur_v"),
            "Flow Field's forceField output is produced by field_blur_v"
        );
        assert_eq!(port, "forceField", "the group's primary output port name");
        assert_eq!(
            crate::node_graph::PreviewEncoding::derive("node.gaussian_blur", &port),
            crate::node_graph::PreviewEncoding::VectorField,
        );
        assert!(
            g.graph.instance_by_node_id(&producer).is_some(),
            "the resolved producer must be a real runtime node"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_fusion_tests {
    //! Cross-card chain fusion integration (docs/CHAIN_FUSION_DESIGN.md):
    //! the per-card build and the fused-segment build of the SAME two-card
    //! chain must render within the pointwise fusion budget of each other,
    //! and the cards' `param_values` must keep driving the fused chain
    //! through the retargeted bindings.

    use super::*;
    use crate::gpu_encoder::GpuEncoder;
    use crate::node_graph::freeze::TextureDiff;
    use crate::node_graph::freeze::install as freeze_install;
    use crate::preset_context::PresetContext;
    use half::f16;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_gpu::{
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    fn set_param(fx: &mut PresetInstance, id: &str, v: f32) {
        let ty = fx.effect_type().clone();
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param id `{id}` on {ty:?}"));
        p.value = v;
        p.base = v;
    }

    fn ctx(w: u32, h: u32) -> PresetContext {
        PresetContext {
            time: 0.5,
            beat: 1.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        }
    }

    fn gradient_input(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-test-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn run_once(
        cg: &mut PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        input: &manifold_gpu::GpuTexture,
        effects: &[PresetInstance],
        pc: &PresetContext,
    ) {
        let mut enc = device.create_encoder("chain-fusion-test");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            cg.run(&mut gpu, input, effects, &[], pc);
        }
        enc.commit_and_wait_completed();
    }

    /// Copy a runtime's current output into a standalone target so a later
    /// run can't overwrite it.
    fn snapshot_output(
        cg: &PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> crate::render_target::RenderTarget {
        let out = cg.output_texture().expect("chain produced output");
        let rt = crate::render_target::RenderTarget::new(
            device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            "chain-fusion-test-snap",
        );
        let mut enc = device.create_encoder("chain-fusion-snap");
        enc.copy_texture_to_texture(out, &rt.texture, w, h, 1);
        enc.commit_and_wait_completed();
        rt
    }

    #[test]
    fn fused_segment_build_matches_per_card_build_and_stays_param_driven() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades with distinct, non-trivial params — the
        // same type twice exercises the segment namespacing on real presets.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        set_param(&mut e1, "gain", 1.2);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        set_param(&mut e2, "gain", 0.85);
        set_param(&mut e2, "saturation", 0.6);
        let effects = vec![e1, e2];

        // ── Per-card build first: the segment cache is cold, the lookup goes
        // Pending (tests never enqueue the worker), and the chain splices
        // per-card — today's production path, our oracle. ──
        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");
        assert_eq!(per_card.effect_nodes.len(), 2);
        assert!(
            per_card.pending_segments,
            "cold cache must leave the chain waiting on the segment compile"
        );
        assert!(
            !per_card.awaiting_segment_swap(),
            "no swap signal until a worker result lands"
        );

        // ── Compile the segment synchronously (the worker's job, minus the
        // gate) and seed the cache, then rebuild: the Ready path. ──
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused-segment chain builds");
        assert_eq!(fused.effect_nodes.len(), 2, "one EffectSlot per card survives");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel"
        );

        // ── Parity at build params. ──
        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment must match per-card chain: max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // ── Live param drive: move card 2's gain on the host slice only. The
        // binding apply path must push it into the fused kernel's uniform. ──
        let before = snapshot_output(&fused, &device, w, h);
        let mut effects2 = effects.clone();
        set_param(&mut effects2[1], "gain", 1.6);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a card slider move must visibly drive the fused segment"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after the slider move the two builds must still agree: max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// BUG-111: an in-place inner-param edit (value/position edit — bumps
    /// `graph_version` only, no rebuild) on a card that is a member of a
    /// fused multi-card SEGMENT must still reach the live kernel. The
    /// segment's `node_map`/`fused_retarget` are keyed with the `c{i}.`
    /// per-card prefix (`freeze::segment::card_prefix`), built from the
    /// concatenated segment def, while the per-frame override path reads
    /// each card's own UNPREFIXED `fx.graph`. Without translating through
    /// that prefix (`EffectSlot::card_prefix` →
    /// `BoundGraph::apply_inner_overrides_prefixed`) the override misses
    /// every node in the map and silently no-ops — the old value keeps
    /// rendering until an unrelated rebuild. Segment sibling of
    /// `bound_graph::inner_override_routes_fused_away_node_through_retarget`.
    #[test]
    fn fused_segment_inner_override_reaches_live_kernel() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades — same fusable two-card segment shape as
        // `fused_segment_build_matches_per_card_build_and_stays_param_driven`.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        let effects = vec![e1, e2];

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [(view1.canonical_def.as_ref(), view1), (view2.canonical_def.as_ref(), view2)];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused-segment chain builds");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel — every one \
             of card 2's inner nodes, including `clamp`, is fused away and \
             only reachable through the segment's retarget map"
        );

        run_once(&mut fused, &device, &input, &effects, &pc);
        let before = snapshot_output(&fused, &device, w, h);

        // Card 2's own (unprefixed) per-instance graph, with `clamp.max`
        // edited to clip the output hard. `clamp` carries no card-slider
        // binding (unlike gain/saturation/contrast/…, which ColorGrade DOES
        // bind — an edit there would just be re-asserted-over by the live
        // binding on the very next apply, proving nothing about the override
        // path itself). Bump `graph_version` only, NOT
        // `graph_structure_version`, so the runtime takes the in-place
        // override path instead of rebuilding.
        let mut effects2 = effects.clone();
        let mut edited = (*view2.canonical_def).clone();
        {
            use manifold_core::effect_graph_def::SerializedParamValue;
            let clamp = edited
                .nodes
                .iter_mut()
                .find(|n| n.node_id.as_str() == "clamp")
                .expect("ColorGrade has a `clamp` node");
            clamp
                .params
                .insert("max".to_string(), SerializedParamValue::Float { value: 0.05 });
        }
        effects2[1].graph = Some(edited);
        effects2[1].bump_graph_version();
        assert_eq!(
            effects2[1].graph_structure_version, effects[1].graph_structure_version,
            "sanity: this must be a value-only edit, not a rebuild"
        );

        run_once(&mut fused, &device, &input, &effects2, &pc);
        let differ = TextureDiff::new(&device);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "an inner-param edit on a fused SEGMENT member must reach the \
             live kernel (BUG-111) — clamping card 2's output to 0.05 must \
             visibly darken the frame: max_abs={}, over={}/{}",
            moved.max_abs,
            moved.over_count,
            moved.total
        );
    }

    /// State harvest (docs/CHAIN_FUSION_DESIGN.md §5): rebuilding a chain
    /// with the prior runtime as donor must carry a feedback trail across the
    /// rebuild — the rebuilt chain continues exactly like a chain that never
    /// rebuilt. A rebuild WITHOUT the donor must visibly reset (sensitivity
    /// check: the trail actually accumulated something worth preserving).
    #[test]
    fn rebuild_with_prior_carries_feedback_trail_across() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // StylizedFeedback (node.feedback trail in the StateStore) followed
        // by a ColorGrade — a realistic dial-in chain. Drive `rotate` so the
        // feedback trail genuinely evolves frame-to-frame: at the default
        // (zoom 0.95, rotate 0) a static self-similar gradient hits a
        // fixed point in one frame, so the output would be frame-invariant
        // and neither the harvest nor the sensitivity check would prove
        // anything. Rotation makes the prev spiral, so frame 1 ≠ frame 9.
        let mut fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_param(&mut fb, "amount", 1.0);
        set_param(&mut fb, "rotate", 10.0);
        set_param(&mut fb, "zoom", 0.9);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let effects = vec![fb, cg];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Three post-rebuild frames, not one: a frozen ping-pong (the
        // shadowed-slot swap-failure class) still shows the carried trail on
        // frame 1 and only diverges once the state should have ADVANCED.
        const TAIL: usize = 3;
        // Reference: never rebuilt, runs WARM+TAIL frames.
        let mut reference = build(None);
        for _ in 0..WARM + TAIL {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Harvested: WARM frames, rebuild WITH the prior, TAIL more frames.
        let mut donor = build(None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let mut harvested = build(Some(&mut donor));
        for _ in 0..TAIL {
            run_once(&mut harvested, &device, &input, &effects, &pc);
        }

        // Reset: WARM frames, rebuild WITHOUT the prior, one more frame.
        let mut fresh_donor = build(None);
        for _ in 0..WARM {
            run_once(&mut fresh_donor, &device, &input, &effects, &pc);
        }
        let mut reset = build(None);
        run_once(&mut reset, &device, &input, &effects, &pc);

        let differ = TextureDiff::new(&device);
        let carried = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            harvested.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            carried.over_count, 0,
            "harvested rebuild must continue the trail exactly like an \
             un-rebuilt chain: max_abs={}, over={}/{}",
            carried.max_abs, carried.over_count, carried.total
        );
        let wiped = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            reset.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            wiped.over_count > 0,
            "sensitivity: a donor-less rebuild must visibly reset the trail \
             (otherwise this test proves nothing)"
        );
    }

    /// Repro harness for the 2026-06-11 on-stage report: Infrared →
    /// QuadMirror fused as a segment washed the frame to the palette's dark
    /// end. Fused segment vs per-card build of the same chain, real GPU.
    #[test]
    fn infrared_quadmirror_segment_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            // No seam-spanning region — nothing fused, nothing to prove.
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared→QuadMirror segment must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Repro for the 2026-06-11 follow-up report: same Infrared → QuadMirror
    /// chain, but with a NON-DEFAULT palette (Arctic, selector 6 — the setting
    /// in the on-stage screenshots). The shipped guard only proves palette 0;
    /// this drives the build-time value and a live palette switch.
    #[test]
    fn infrared_quadmirror_segment_nondefault_palette() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared(Arctic)→QuadMirror must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // Live palette switch on the fused chain: 6 → 2 (Green NV) must both
        // visibly change the output and still match per-card.
        let mut effects2 = effects.clone();
        set_param(&mut effects2[0], "palette", 2.0);
        let before = snapshot_output(&fused, &device, w, h);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a live palette switch must visibly drive the fused chain"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after a live palette switch the fused chain must match per-card: \
             max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// Wireframe-like input: transparent black background (alpha 0), thin
    /// opaque white lines — the content class from the 2026-06-11 screenshots
    /// (generator wireframes), where Infrared→QuadMirror killed the frame but
    /// QuadMirror→Infrared rendered. The gradient repro (alpha 1 everywhere)
    /// passes, so alpha across the fused seam is the variable under test.
    fn wireframe_input(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let on_line = x % 32 < 2 || y % 32 < 2;
                let v = if on_line { 1.0 } else { 0.0 };
                px[i] = f16::from_f32(v);
                px[i + 1] = f16::from_f32(v);
                px[i + 2] = f16::from_f32(v);
                px[i + 3] = f16::from_f32(v); // alpha 0 off-line, like a generator
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-wireframe-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Same fused-vs-per-card proof on the wireframe-like (alpha-0 background)
    /// input, both chain orders.
    #[test]
    fn infrared_quadmirror_segment_alpha_zero_background() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Deliberately NOT 256x256: the gradient_ramp LUT strip is 256 wide,
        // and a 256 canvas can mask cross-resolution sampling bugs by making
        // strip texels and canvas texels coincide.
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        for order in ["ir_qm", "qm_ir"] {
            let mut ir = make_default(PresetTypeId::INFRARED);
            set_param(&mut ir, "amount", 1.0);
            set_param(&mut ir, "palette", 6.0); // Arctic
            let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
            set_param(&mut qm, "amount", 1.0);
            let effects = if order == "ir_qm" {
                vec![ir, qm]
            } else {
                vec![qm, ir]
            };

            let mut per_card = PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, None,
            )
            .expect("per-card chain builds");

            let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
            let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
            let cards = [
                (view1.canonical_def.as_ref(), view1),
                (view2.canonical_def.as_ref(), view2),
            ];
            if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
                continue;
            }
            let mut fused = PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, None,
            )
            .expect("fused chain builds");

            // Several STABLE frames: static-param specialization compiles a
            // baked variant once the value-key holds a frame and dispatches it
            // from then on — the steady-state path a live show actually runs.
            // One frame would only ever prove the generic kernel.
            for _ in 0..4 {
                run_once(&mut per_card, &device, &input, &effects, &pc);
                run_once(&mut fused, &device, &input, &effects, &pc);
            }
            let differ = TextureDiff::new(&device);
            let r = differ.compare(
                &device,
                per_card.output_texture().unwrap(),
                fused.output_texture().unwrap(),
                1.0e-2,
                3.0e-2,
            );
            assert!(
                r.passes(0.005) && r.over_count < 64,
                "[{order}] fused must match per-card on alpha-0 background: \
                 max_abs={}, over={}/{}",
                r.max_abs,
                r.over_count,
                r.total
            );
        }
    }

    /// The PRODUCTION swap sequence, end-to-end: build per-card (cold segment
    /// cache), render frames, the background compile lands, rebuild WITH the
    /// running chain as harvest donor, fused segment swaps in, keep rendering.
    /// The shipped guards seed the cache BEFORE the first build, so the
    /// mid-show swap-in (the path the app actually takes) was never proven.
    #[test]
    fn infrared_quadmirror_mid_show_swap_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        // Per-card reference, never swapped.
        let mut reference = build(None);
        for _ in 0..6 {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Production path: per-card frames, then the segment compile lands
        // and the chain rebuilds with the outgoing runtime as donor.
        let mut donor = build(None);
        for _ in 0..3 {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut swapped = build(Some(&mut donor));
        for _ in 0..3 {
            run_once(&mut swapped, &device, &input, &effects, &pc);
        }

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            swapped.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "mid-show fused swap must match the per-card chain: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// The GraphTestsV4 layer-1 shape: a DISABLED card sits between the two
    /// enabled cards that fuse (Infrared ON → EdgeStretch OFF → QuadMirror ON).
    /// Segment fusion concatenates enabled cards across the gap; anything that
    /// indexes params by raw chain position would hand the fused kernel the
    /// disabled card's uniforms.
    #[test]
    fn fused_segment_spans_disabled_card_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut es = make_default(PresetTypeId::EDGE_STRETCH);
        es.enabled = false;
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, es, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[2].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment spanning a disabled card must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Chain input at a DIFFERENT resolution than the canvas — the app feeds
    /// the chain a generator render target, which the resolution workstream
    /// can size below canvas. The fused kernel reads the chain source as an
    /// external (the cross-resolution sampling path); per-card resamples it
    /// node by node. An unfused QuadMirror in front normalizes resolution and
    /// would mask exactly this class, matching the order dependence reported.
    #[test]
    fn fused_segment_with_half_res_chain_input_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w / 2, h / 2);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment with half-res chain input must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// "Flash for a few frames then black" repro (2026-06-12, fusion OFF):
    /// Infrared ALONE on a STATIC input must produce a byte-identical frame
    /// every frame — it has no time dependence. The memo/hoisting path
    /// (gradient_ramp/mux/lut1d are pure+sticky) serves held LUT slots after
    /// the first frame; if a held slot is recycled/evicted/cleared the late
    /// frames go black while frame 0 was correct. Snapshot frame 0, run many
    /// frames, require the late frame to still match.
    #[test]
    fn infrared_alone_static_input_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let effects = vec![ir];

        let mut cg = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("chain builds");

        // Frame 0 — the "flash" that looks correct.
        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        // Many more frames — the memo/sticky path is now serving held slots.
        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared on a static input must be frame-stable; a late frame \
             diverging from frame 0 is the flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// The on-stage blackout (2026-06-12): Infrared FOLLOWED BY another card,
    /// fusion off, static input. The chain plan's slot planner returns the
    /// sticky LUT resources' slots to its free pool at `free_after` (it only
    /// exempts persistent resources), so QuadMirror's intermediates share the
    /// LUT's physical texture and stomp it every frame — while the executor's
    /// memo skip keeps serving the latched slot. Infrared LAST works by
    /// accident (nothing runs after it to reuse the slot); this ordering is
    /// the one that goes black.
    #[test]
    fn infrared_before_quadmirror_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let qm = make_default(PresetTypeId::QUAD_MIRROR);
        let effects = vec![ir, qm];

        let mut cg = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("chain builds");

        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared before QuadMirror on a static input must be frame-stable; \
             late-frame divergence is the on-stage flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// Membership gate: a rebuild whose ACTIVE CARD SET changed (a card
    /// toggled off) must NOT harvest — the trail holds the removed card's
    /// look, and latching blends would freeze it on screen with no escape
    /// (the on-stage artifact class from 2026-06-11). Same-set rebuilds keep
    /// carrying.
    #[test]
    fn toggle_rebuild_resets_state_same_set_rebuild_carries() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let both = vec![fb.clone(), cg.clone()];
        // The toggled chain: ColorGrade disabled → not an active card.
        let mut cg_off = cg.clone();
        cg_off.enabled = false;
        let toggled = vec![fb.clone(), cg_off];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Donor accumulates a trail through BOTH cards, then the chain
        // rebuilds with ColorGrade toggled off.
        let mut donor = build(&both, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &both, &pc);
        }
        let mut after_toggle = build(&toggled, Some(&mut donor));
        run_once(&mut after_toggle, &device, &input, &toggled, &pc);

        // Oracle: the toggled chain built fresh (what a reset looks like).
        let mut fresh = build(&toggled, None);
        run_once(&mut fresh, &device, &input, &toggled, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            after_toggle.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "a toggle rebuild must reset state (match a fresh build), not \
             carry the old trail: max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }

    /// Upstream-prefix gate: moving a card UPSTREAM of a stateful card
    /// changes what feeds it — its carried trail would be a stale picture of
    /// the old chain (the 2026-06-11 reorder artifact). The rebuild must
    /// reset exactly that card: [FB, CG] reordered to [CG, FB] makes the
    /// harvested chain match a fresh [CG, FB] build.
    #[test]
    fn upstream_reorder_resets_stateful_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let fb_first = vec![fb.clone(), cg.clone()];
        let cg_first = vec![cg.clone(), fb.clone()];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        let mut donor = build(&fb_first, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &fb_first, &pc);
        }
        let mut reordered = build(&cg_first, Some(&mut donor));
        run_once(&mut reordered, &device, &input, &cg_first, &pc);

        let mut fresh = build(&cg_first, None);
        run_once(&mut fresh, &device, &input, &cg_first, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            reordered.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "an upstream reorder must reset the feedback card (match a fresh \
             build): max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }
}

#[cfg(test)]
mod segment_prewarm_tests {
    //! Project-load segment prewarm shares `classify_segment_member` /
    //! `segment_run` with the chain build — these lock the shared pieces.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn segment_run_trims_transparents_and_collects_fuse_indices() {
        use SegmentMember::{Boundary, Fuse, Transparent};
        // Fuse, Transparent, Fuse, Transparent, Boundary: run ends at the
        // boundary, the trailing transparent is trimmed, fuse idxs = [0, 2].
        let members = [Fuse, Transparent, Fuse, Transparent, Boundary];
        let (j, fuse) = segment_run(&members, 0);
        assert_eq!(j, 3, "trailing transparent trimmed back to a plain card");
        assert_eq!(fuse, vec![0, 2]);
    }

    #[test]
    fn prewarm_classifies_stateless_cards_fuse_and_enqueues_without_panicking() {
        let primitives = PrimitiveRegistry::with_builtin();
        let a = make_default(PresetTypeId::COLOR_GRADE);
        let b = make_default(PresetTypeId::INVERT_COLORS);
        assert_eq!(
            classify_segment_member(&a, None, &primitives),
            SegmentMember::Fuse,
            "ColorGrade is a stateless ungrouped card — segment member"
        );
        // A watched card is a boundary — prewarm passes None so nothing is
        // watched at load, but the build-time exclusion must hold.
        assert_eq!(
            classify_segment_member(&a, Some(&a.id), &primitives),
            SegmentMember::Boundary
        );
        // Enqueue-only walk; in cfg(test) the segment lookup stays Pending
        // (no worker) — this locks that the walk itself is panic-free and
        // exercises the same card-list construction as the build.
        prewarm_chain_segments(&[a, b], &[], &primitives);
    }
}
