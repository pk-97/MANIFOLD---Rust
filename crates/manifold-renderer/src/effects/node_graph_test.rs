//! Node Graph Test — minimal proof-of-life splice (`Mix` of source with
//! itself).
//!
//! Predates the chain-splice path; the original `NodeGraphTestFX` used
//! hardcoded red/blue test sources, which the splice protocol can't
//! reproduce (a spliced effect always reads from the previous stage's
//! output). The current spec keeps the entry in the effect catalog and
//! the `amount` slider functional — useful when validating new
//! plumbing — but visually it's a passthrough.

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::NODE_GRAPH_TEST,
        display_name: "Node Graph Test",
        category: "Diagnostic",
        available: true,
        osc_prefix: "node_graph_test",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

