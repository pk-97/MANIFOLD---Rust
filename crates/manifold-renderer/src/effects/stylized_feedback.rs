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
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Screen", "Add", "Max"], "Mode"),
        ],
    }
}

/// Mode remap: host slider (0=Screen / 1=Add / 2=Max) lines up 1:1
/// with Feedback's blend enum. Kept explicit so the convention is
/// visible at the spec.
const STYLIZED_FEEDBACK_MODE_REMAP: &[u32] = &[0, 1, 2];

crate::atomic_chain_spec! {
    type_id: EffectTypeId::STYLIZED_FEEDBACK,
    primitive: Feedback,
    handle: "feedback",
    input_port: "source",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            target: ParamTarget::HandleNode { handle: "feedback", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("zoom"),
            spec: ParamSpec::continuous("zoom", "Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
            target: ParamTarget::HandleNode { handle: "feedback", param: "zoom" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("rotate"),
            spec: ParamSpec::continuous("rotate", "Rotate", -10.0, 10.0, 0.0, "F2", "Rotate"),
            target: ParamTarget::HandleNode { handle: "feedback", param: "rotation" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("mode"),
            spec: ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Screen", "Add", "Max"], "Mode"),
            target: ParamTarget::HandleNode { handle: "feedback", param: "mode" },
            convert: ParamConvert::EnumRemap(Cow::Borrowed(STYLIZED_FEEDBACK_MODE_REMAP)),
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}
