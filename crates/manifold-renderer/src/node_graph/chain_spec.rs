//! Chain-graph splicing primitives.
//!
//! After the §11 unified-registry migration, every shipping effect is
//! a JSON `EffectGraphDef` consumed via [`crate::node_graph::LoadedPresetView`].
//! The chain build loop calls [`splice_def_into_chain`] with each
//! active effect's canonical (or per-instance overridden) def to graft
//! its worker subgraph into the shared chain graph; [`is_skipped_for`]
//! decides whether to skip an effect entirely based on its declared
//! [`SkipMode`].
//!
//! This module holds the small set of types both paths share —
//! [`SpliceResult`], [`SkipMode`], and the splice/skip fns themselves.
//! The legacy `ChainSpec` inventory channel that previously lived here
//! is gone (block 8); the file name persists for the moment so the
//! re-export surface in `node_graph/mod.rs` can stay stable through
//! the rest of §11.

use std::borrow::Cow;

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::{PresetInstance, RelightParams};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::graph_loader::{
    BoundaryHandling, HandleScope, instantiate_def, log_build_error,
};
use crate::node_graph::persistence::PrimitiveRegistry;

/// Outcome of a single splice into the chain graph.
pub struct SpliceResult {
    /// `(node, port)` where the spliced effect's output lives. Port
    /// names come from primitive port declarations and are always
    /// `&'static str`.
    pub output: (NodeInstanceId, &'static str),

    /// Effect-local handle map. Names are scoped to the spliced
    /// effect; bindings + user-bindings look up nodes here, never on
    /// the chain graph globally.
    ///
    /// `Cow<'static, str>` so canonical splices can carry compile-time
    /// literals as `Cow::Borrowed("mix")` while JSON-loaded defs use
    /// `Cow::Owned` for names that came off disk.
    pub handles: Vec<(Cow<'static, str>, NodeInstanceId)>,

    /// The spliced subgraph's `system.generator_input` node id, if the
    /// preset included one. The chain runner pushes per-frame scalars
    /// (time / beat / aspect / output dims) to this node so effects
    /// can wire project timing through the standard port-shadows-param
    /// machinery — the same surface generators have. `None` when the
    /// preset has no `system.generator_input` (the case for almost
    /// every shipping effect today; opt-in as effects migrate off the
    /// hardcoded `apply_ctx_params_at` path).
    pub generator_input_id: Option<NodeInstanceId>,
}

/// When the chain should drop an effect entirely (no workers added,
/// no cost). Previous output flows directly to the next effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMode {
    /// Effect always contributes its workers.
    Never,
    /// Skip when the param identified by `param_id` is ≤ 0.
    OnZero { param_id: &'static str },
}

/// Standalone skip check used by the JSON-loaded preset path
/// (`LoadedPresetView`). Lookup goes through
/// `preset_definition_registry::param_id_to_index` which is
/// dual-source aware — works for both inventory-submitted
/// `EffectMetadata` and JSON-loaded `PresetMetadata`.
pub fn is_skipped_for(skip: SkipMode, _type_id: &PresetTypeId, fx: &PresetInstance) -> bool {
    match skip {
        SkipMode::Never => false,
        SkipMode::OnZero { param_id } => fx
            .params
            .get(param_id.as_ref())
            .map(|p| p.value <= 0.0)
            .unwrap_or(false),
    }
}

/// Splice an [`EffectGraphDef`] into the chain graph. Used by both the
/// canonical path (with `view.canonical_def` from
/// [`crate::node_graph::LoadedPresetView`]) and the per-card override
/// path (with `PresetInstance.graph`). Returns the output endpoint +
/// effect-local handle map.
///
/// The def's `Source` boundary disappears — every wire fanning out
/// from it is re-anchored to `source` (the chain's previous endpoint).
/// The def's `FinalOutput` boundary also disappears — the wire feeding
/// into it identifies the def's output endpoint, which becomes the
/// chain's next source.
///
/// Per-node params encoded in the def (the user's slider edits) are
/// applied via `graph.set_param` before returning. Effect-local
/// handles (named nodes in the def) flow into [`SpliceResult::handles`]
/// so routings + user-bindings resolve uniformly.
///
/// Returns `None` on malformed input (no Source / no FinalOutput /
/// unknown type id / orphan wire).
///
/// `relight` is the "3D Shading" toggle at the compile level
/// (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5): `Some(params)` passes `def`
/// through [`crate::node_graph::relight::relight_augment`] before splicing —
/// the depth-companion synthesis + fixed relight template (parameterized by
/// the instance's live `RelightParams`) appended before `final_output`.
/// `None` is the exact unaugmented def, byte-identical to pre-P3 behavior —
/// the wire this function's callers use for every def with no per-instance
/// toggle in scope (tests, thumbnails, the no-override registry path).
pub fn splice_def_into_chain(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    relight: Option<&RelightParams>,
) -> Option<SpliceResult> {
    let augmented;
    let def = if let Some(params) = relight {
        augmented = crate::node_graph::relight::relight_augment(def, registry, params);
        &augmented
    } else {
        def
    };

    // Delegate every per-node + per-wire concern to the shared
    // graph_loader pipeline. The same pipeline runs for the generator
    // path (`persistence::into_graph`), so any per-node feature added
    // here automatically applies there too — the drift bug class is
    // structurally eliminated. Validation errors (unknown type_id,
    // param mismatch, output-format-not-supported, etc.) bubble up as
    // structured `GraphBuildError`s; the chain build's policy is to
    // log + fall back, preserving the legacy `Option<SpliceResult>`
    // contract.
    let inst = match instantiate_def(
        graph,
        def,
        registry,
        HandleScope::PerSplice,
        BoundaryHandling::Splice {
            source_endpoint: source,
        },
    ) {
        Ok(i) => i,
        Err(e) => {
            let context = def
                .name
                .as_deref()
                .or_else(|| def.preset_metadata.as_ref().map(|m| m.id.as_str()))
                .unwrap_or("<unnamed splice def>");
            log_build_error(context, &e);
            return None;
        }
    };

    Some(SpliceResult {
        output: inst.output_endpoint?,
        handles: inst.effect_local_handles,
        generator_input_id: inst.generator_input_id,
    })
}
