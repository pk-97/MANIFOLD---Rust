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

use crate::node_graph::primitives::{FEEDBACK_MODES, Feedback};
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};

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
            ParamSpec::whole_labels(
                "mode",
                "Mode",
                0.0,
                (FEEDBACK_MODES.len() - 1) as f32,
                0.0,
                FEEDBACK_MODES,
                "Mode",
            ),
        ],
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::STYLIZED_FEEDBACK,
    primitive: Feedback,
    handle: "feedback",
    input_port: "source",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.5,
            target: ParamTarget::HandleNode { handle: "feedback", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("zoom"),
            label: "Zoom",
            default_value: 0.95,
            target: ParamTarget::HandleNode { handle: "feedback", param: "zoom" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("rotate"),
            label: "Rotate",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "feedback", param: "rotation" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("mode"),
            label: "Mode",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "feedback", param: "mode" },
            convert: ParamConvert::EnumRound,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}
