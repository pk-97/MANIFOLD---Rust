//! Structured load/run error types for [`PresetRuntime`] — the generator
//! load errors and the chain-runner diagnostics. Extracted from
//! preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

mod generator_load;
pub use generator_load::JsonGeneratorLoadError;

/// Structured error variants the chain runner produces. Every variant
/// carries the affected effect's identity so the future editor surface
/// can highlight the right card / node. Today this drives the
/// consistent `[chain-error]` terminal log; tomorrow it's the data
/// the editor reads via [`PresetRuntime::errors`].
#[derive(Debug, Clone)]
pub enum ChainError {
    PreparedParameterChanged {
        node_id: String,
        param: String,
    },
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
    PreAllocationFailed {
        reason: String,
    },
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
    /// BUG-1l7f: a value baked onto a node param that an outer card owns was
    /// thrown away at build — [`crate::node_graph::BoundGraph::new`] plants the
    /// card binding's declared default over it, so the def value never reaches
    /// evaluation. Detected by
    /// [`crate::node_graph::find_shadowed_def_params`] on every chain and
    /// generator build, which is where the writer's mistake is; before this, the
    /// only symptom was a wrong measurement days later (`rt_r3_heldout_gltf` ran
    /// its whole life comparing two pure-raster renders because it set
    /// `rt_enabled` on an imported def's `render_scene` node). Not a build
    /// failure — the graph still runs, on the card's value.
    CardBindingShadowsDefParam {
        effect_type: Option<PresetTypeId>,
        finding: crate::node_graph::ShadowedDefParam,
    },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreparedParameterChanged { node_id, param } => write!(
                f,
                "{node_id}.{param}: prepared source or render mode changed; restore it or reapply the modifier. Rendering is suspended."
            ),
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
                 docs/DECOMPOSING_GENERATORS.md section 4.1's trigger_modulate idiom: switch_value with \
                 an identity default on the idle branch + a downstream math node), or if this is a \
                 genuine discrete selector, add it to trigger_shadow_lint::DISCRETE_REPLACE_ALLOWLIST \
                 and record the decision in this preset's description."
            ),
            Self::CardBindingShadowsDefParam {
                effect_type,
                finding,
            } => {
                let who = match effect_type {
                    Some(t) => format!("{}: ", t.as_str()),
                    None => String::new(),
                };
                write!(f, "{who}{finding} (BUG-1l7f)")
            }
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
