//! Mirror — exposes `Transform` + `Mix` as a single chain card.
//!
//! ```text
//! Source ──▶ Transform[mode=FoldX, …] ──▶ Mix.b
//! Source ──────────────────────────────▶ Mix.a
//! Mix.out ─────────────────────────────▶ next stage
//! ```
//!
//! Mirror's `mode` slider exposes the full `TRANSFORM_MODES` enum
//! (9 options: Identity, Mirror, MirrorX, MirrorY, FlipY, QuadMirror,
//! FoldX, FoldY, FoldBoth). The preset defaults to `FoldX` because
//! that's the legacy "mirror across the X axis" behavior, but the
//! user can switch to any other Transform mode without leaving the
//! card. Legacy projects authored under the curated 3-mode slider
//! (Horiz=0 / Vert=1 / Both=2) migrate at load time via
//! `legacy_value_aliases` below: 0→6, 1→7, 2→8.
//!
//! No GPU-side processor — the splice plants Transform + Mix workers
//! directly in the chain graph and the per-frame renderer drives
//! them via the bindings below.

use std::borrow::Cow;

use crate::node_graph::primitives::{Mix, TRANSFORM_MODES, Transform};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamBinding, ParamConvert, ParamTarget, ParamValue,
    SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::{EffectMetadata, EffectValueAliasMetadata};
use manifold_core::generator_registration::ParamSpec;

/// Transform mode index for FoldX. Matches `TRANSFORM_MODES`.
const MIRROR_FOLD_X: u32 = 6;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::MIRROR,
        display_name: "Mirror",
        category: "Post-Process",
        available: true,
        osc_prefix: "mirror",
        legacy_discriminant: Some(21),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::whole_labels(
                "mode",
                "Mode",
                0.0,
                (TRANSFORM_MODES.len() - 1) as f32,
                MIRROR_FOLD_X as f32,
                TRANSFORM_MODES,
                "Mode",
            ),
        ],
    }
}

// Legacy `Mirror.mode` migration table. Pre-unification the outer
// slider was a curated 3-option enum (Horiz=0 / Vert=1 / Both=2)
// translated to the inner Transform enum via
// `ParamConvert::EnumRemap([6, 7, 8])`. After dropping the curation
// the outer value IS the inner value, so old saves with `mode ∈
// {0,1,2}` need a one-time rewrite to `{6,7,8}` at load.
inventory::submit! {
    EffectValueAliasMetadata {
        id: EffectTypeId::MIRROR,
        aliases: &[
            ("mode", &[(0, 6), (1, 7), (2, 8)]),
        ],
    }
}

/// Splice Mirror's workers (`Transform` + `Mix`) directly into a chain
/// graph. The source endpoint fans out — `Transform` reads it as
/// `source`, `Mix` reads it as `a` (dry path). `Mix.amount` lerps
/// between the source and the folded result.
fn splice_mirror(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> SpliceResult {
    let xform = graph.add_node(Box::new(Transform::new()));
    graph
        .set_param(xform, "mode", ParamValue::Enum(MIRROR_FOLD_X))
        .expect("Transform exposes a `mode` enum param");
    graph
        .connect(source, (xform, "source"))
        .expect("wire source → Transform.source");

    let mix = graph.add_node(Box::new(Mix::new()));
    graph
        .connect(source, (mix, "a"))
        .expect("wire source → Mix.a");
    graph
        .connect((xform, "out"), (mix, "b"))
        .expect("wire Transform.out → Mix.b");

    SpliceResult {
        output: (mix, "out"),
        handles: vec![
            (Cow::Borrowed("uv_transform"), xform),
            (Cow::Borrowed("mix"), mix),
        ],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::MIRROR,
        splice: splice_mirror,
        bindings: &[
            ParamBinding {
                id: Cow::Borrowed("amount"),
                spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
                target: ParamTarget::HandleNode { handle: "mix", param: "amount" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("mode"),
                spec: ParamSpec::whole_labels(
                    "mode",
                    "Mode",
                    0.0,
                    (TRANSFORM_MODES.len() - 1) as f32,
                    MIRROR_FOLD_X as f32,
                    TRANSFORM_MODES,
                    "Mode",
                ),
                target: ParamTarget::HandleNode { handle: "uv_transform", param: "mode" },
                convert: ParamConvert::EnumRound,
            },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
