//! Structured load/run error types for [`PresetRuntime`] — the generator
//! load errors and the chain-runner diagnostics. Extracted from
//! preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

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
/// `[chain-error]` line. Replaces the scattered `eprintln!` calls —
/// same data lands in the log, plus
/// it's now reachable through [`PresetRuntime::errors`] for the editor.
pub(super) fn record_chain_error(errors: &mut Vec<ChainError>, err: ChainError) {
    eprintln!("[chain-error] {err}");
    errors.push(err);
}
