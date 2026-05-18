//! Edge Stretch By Colour — first demo of the Phase A mask primitives.
//!
//! ```text
//! Source ─┬─────────────────────────────────────┬─→ MaskedMix.a    (untouched)
//!         ├─→ ChromaKey ──────────────────────────→ MaskedMix.mask (where to apply)
//!         └─→ ClampStretch ───────────────────────→ MaskedMix.b    (effect)
//!                                                 MaskedMix.out → next stage
//! ```
//!
//! ChromaKey produces a mask isolating pixels close to a target colour
//! (red by default — the colour lives inside the graph, editable from
//! the graph canvas). ClampStretch produces the stretched version of
//! the whole frame. MaskedMix uses the chroma mask to blend the
//! stretched version on top of the original — so the stretch is only
//! visible where the colour matches.
//!
//! Outer-card sliders:
//! - `amount`     → MaskedMix.amount (gate; effect skips at 0)
//! - `tolerance`  → ChromaKey.tolerance (how wide the colour band is)
//! - `softness`   → ChromaKey.softness (mask edge falloff)
//! - `stretch`    → ClampStretch.source_width (effect intensity)
//!
//! The key colour (default red) and stretch direction (default
//! horizontal) live inside the graph — users edit them from the
//! graph canvas, not the effect card. That's the intended pattern
//! for advanced composition: card = curated performance surface,
//! canvas = full control.

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::primitives::{ChromaKey, ClampStretch, MaskedMix};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamBinding, ParamConvert, ParamTarget, SkipMode,
    SpliceResult,
};

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::EDGE_STRETCH_BY_COLOR,
        display_name: "Edge Stretch By Colour",
        category: "Stylize",
        available: true,
        osc_prefix: "edge_stretch_by_color",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount",    "Amount",    0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::continuous("tolerance", "Tolerance", 0.0, 1.0, 0.3, "F2", ""),
            ParamSpec::continuous("softness",  "Softness",  0.0, 1.0, 0.1, "F2", ""),
            ParamSpec::continuous("stretch",   "Stretch",   0.1, 0.9, 0.5, "F2", ""),
        ],
    }
}

fn splice_edge_stretch_by_color(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> SpliceResult {
    // Mask path: source → chroma_key
    let chroma = graph.add_node(Box::new(ChromaKey::new()));
    graph
        .connect(source, (chroma, "in"))
        .expect("wire source → ChromaKey.in");

    // Effect path: source → clamp_stretch
    let stretch = graph.add_node(Box::new(ClampStretch::new()));
    graph
        .connect(source, (stretch, "in"))
        .expect("wire source → ClampStretch.in");

    // Masked composite: a=source (pass-through), b=stretched, mask=chroma
    let mm = graph.add_node(Box::new(MaskedMix::new()));
    graph
        .connect(source, (mm, "a"))
        .expect("wire source → MaskedMix.a");
    graph
        .connect((stretch, "out"), (mm, "b"))
        .expect("wire ClampStretch.out → MaskedMix.b");
    graph
        .connect((chroma, "out"), (mm, "mask"))
        .expect("wire ChromaKey.out → MaskedMix.mask");

    SpliceResult {
        output: (mm, "out"),
        handles: vec![
            (Cow::Borrowed("chroma_key"), chroma),
            (Cow::Borrowed("clamp_stretch"), stretch),
            (Cow::Borrowed("masked_mix"), mm),
        ],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::EDGE_STRETCH_BY_COLOR,
        splice: splice_edge_stretch_by_color,
        bindings: &[
            ParamBinding {
                id: Cow::Borrowed("amount"),
                label: "Amount",
                default_value: 1.0,
                target: ParamTarget::HandleNode { handle: "masked_mix", param: "amount" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("tolerance"),
                label: "Tolerance",
                default_value: 0.3,
                target: ParamTarget::HandleNode { handle: "chroma_key", param: "tolerance" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("softness"),
                label: "Softness",
                default_value: 0.1,
                target: ParamTarget::HandleNode { handle: "chroma_key", param: "softness" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("stretch"),
                label: "Stretch",
                default_value: 0.5,
                target: ParamTarget::HandleNode { handle: "clamp_stretch", param: "source_width" },
                convert: ParamConvert::Float,
            },
        ],
        // Skip when the masked-mix gate is off — saves the chroma-key
        // and clamp-stretch passes when the user pulls amount to 0.
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
