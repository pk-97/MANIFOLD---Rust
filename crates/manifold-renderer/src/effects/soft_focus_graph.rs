//! Soft Focus — separable Gaussian blur composited back over the original.
//!
//! ```text
//! Source ──▶ Blur ──▶ Mix.b
//! Source ───────────▶ Mix.a
//! Mix.out ─────────▶ next stage
//! ```
//!
//! Exposes:
//! - `radius` → `Blur.radius` (0..32, shader caps internally)
//! - `amount` → `Mix.amount` (0 = sharp original, 1 = full blur)

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::primitives::{Blur, Mix};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::SOFT_FOCUS_GRAPH,
        display_name: "Soft Focus",
        category: "Post-Process",
        available: true,
        osc_prefix: "soft_focus_graph",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("radius", "Radius", 0.0, 32.0, 6.0, "F1", "px"),
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

fn splice_soft_focus(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let blur = graph.add_node(Box::new(Blur::new()));
    graph.connect(source, (blur, "source")).expect("wire source → Blur.source");

    let mix = graph.add_node(Box::new(Mix::new()));
    graph.connect(source, (mix, "a")).expect("wire source → Mix.a");
    graph.connect((blur, "out"), (mix, "b")).expect("wire Blur.out → Mix.b");

    SpliceResult {
        output: (mix, "out"),
        handles: vec![(Cow::Borrowed("blur"), blur), (Cow::Borrowed("mix"), mix)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::SOFT_FOCUS_GRAPH,
        splice: splice_soft_focus,
        routings: &[
            Routing { param_id: "radius", target_handle: "blur", target_param: "radius", convert: ParamConvert::Float },
            Routing { param_id: "amount", target_handle: "mix", target_param: "amount", convert: ParamConvert::Float },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
