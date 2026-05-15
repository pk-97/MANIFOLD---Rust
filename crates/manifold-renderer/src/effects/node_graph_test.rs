//! Node Graph Test — minimal proof-of-life splice (`Mix` of source with
//! itself).
//!
//! Predates the chain-splice path; the original `NodeGraphTestFX` used
//! hardcoded red/blue test sources, which the splice protocol can't
//! reproduce (a spliced effect always reads from the previous stage's
//! output). The current spec keeps the entry in the effect catalog and
//! the `amount` slider functional — useful when validating new
//! plumbing — but visually it's a passthrough.

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::NODE_GRAPH_TEST,
        display_name: "Node Graph Test",
        category: "Post-Process",
        available: true,
        osc_prefix: "node_graph_test",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

fn splice_node_graph_test(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let mix = graph.add_node(Box::new(Mix::new()));
    graph.connect(source, (mix, "a")).expect("wire source → Mix.a");
    graph.connect(source, (mix, "b")).expect("wire source → Mix.b");
    SpliceResult {
        output: (mix, "out"),
        handles: vec![(Cow::Borrowed("mix"), mix)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::NODE_GRAPH_TEST,
        splice: splice_node_graph_test,
        routings: &[
            Routing { param_id: "amount", target_handle: "mix", target_param: "amount", convert: ParamConvert::Float },
        ],
        skip: SkipMode::Never,
    }
}
