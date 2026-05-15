//! Mirror — axis-aligned UV fold (Horiz / Vert / Both).
//!
//! Composite of `Transform[mode=Foldᴹ]` and `Mix`:
//!
//! ```text
//! Source ──▶ Transform[mode=Foldᴹ] ──▶ Mix.b
//! Source ───────────────────────────────▶ Mix.a
//! Mix.out ─────────────────────────────▶ next stage
//! ```
//!
//! The ChainSpec routings translate the legacy mode slider (0=Horiz /
//! 1=Vert / 2=Both) onto the Transform primitive's enum (6=FoldX /
//! 7=FoldY / 8=FoldBoth). No GPU-side processor — the splice plants
//! Transform + Mix workers directly in the chain graph and the
//! per-frame renderer drives them via the routings below.

use std::borrow::Cow;

use crate::node_graph::primitives::{Mix, Transform};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, ParamValue, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

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
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Mode"),
        ],
    }
}

/// Transform mode index for FoldX. Matches `TRANSFORM_MODES`.
const MIRROR_FOLD_X: u32 = 6;

/// Legacy mode (0=Horiz / 1=Vert / 2=Both) → Transform mode enum
/// (6=FoldX / 7=FoldY / 8=FoldBoth). Indexed by the host slider value.
const MIRROR_MODE_REMAP: &[u32] = &[6, 7, 8];

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
        routings: &[
            Routing {
                param_id: "amount",
                target_handle: "mix",
                target_param: "amount",
                convert: ParamConvert::Float,
            },
            Routing {
                param_id: "mode",
                target_handle: "uv_transform",
                target_param: "mode",
                convert: ParamConvert::EnumRemap(Cow::Borrowed(MIRROR_MODE_REMAP)),
            },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
