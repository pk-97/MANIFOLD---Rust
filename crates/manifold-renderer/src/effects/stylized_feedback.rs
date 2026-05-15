//! Stylized Feedback — per-owner persistent feedback loop with zoom + rotate.
//!
//! `Source → Feedback → next stage`. Per-owner prev-frame state lives
//! in the chain's `StateStore`, keyed by `(NodeInstanceId, OwnerKey)` —
//! the splice runtime manages the lifecycle.
//!
//! Exposes 4 params:
//! - `amount` → `Feedback.amount` (0 = passthrough)
//! - `zoom`   → `Feedback.zoom`
//! - `rotate` → `Feedback.rotation`
//! - `mode`   → `Feedback.mode` (Screen / Add / Max)

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::primitives::Feedback;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::STYLIZED_FEEDBACK,
        display_name: "Stylized Feedback",
        category: "Post-Process",
        available: true,
        osc_prefix: "stylizedFeedback",
        legacy_discriminant: Some(20),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::continuous("zoom", "Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
            ParamSpec::continuous("rotate", "Rotate", -10.0, 10.0, 0.0, "F2", "Rotate"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Screen", "Add", "Max"], "Mode"),
        ],
    }
}

fn splice_stylized_feedback(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(Feedback::new()));
    graph.connect(source, (node, "source")).expect("wire source → Feedback.source");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("feedback"), node)],
    }
}

/// Mode remap: host slider (0=Screen / 1=Add / 2=Max) lines up 1:1
/// with Feedback's blend enum. Kept explicit so the convention is
/// visible at the spec.
const STYLIZED_FEEDBACK_MODE_REMAP: &[u32] = &[0, 1, 2];

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::STYLIZED_FEEDBACK,
        splice: splice_stylized_feedback,
        routings: &[
            Routing { param_id: "amount", target_handle: "feedback", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "zoom", target_handle: "feedback", target_param: "zoom", convert: ParamConvert::Float },
            Routing { param_id: "rotate", target_handle: "feedback", target_param: "rotation", convert: ParamConvert::Float },
            Routing { param_id: "mode", target_handle: "feedback", target_param: "mode", convert: ParamConvert::EnumRemap(Cow::Borrowed(STYLIZED_FEEDBACK_MODE_REMAP)) },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
