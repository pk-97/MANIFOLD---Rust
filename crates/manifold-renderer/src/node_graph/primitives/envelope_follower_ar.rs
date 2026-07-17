//! `node.envelope_follower_ar` — asymmetric attack/release envelope
//! on a scalar input.
//!
//! Two-coefficient exponential filter where the time constant
//! switches based on whether the input is rising or falling. Rising
//! → use `attack`; falling → use `release`. The existing
//! `node.smoothing` primitive is symmetric (single time constant),
//! so this exists for audio-style envelope use cases that need
//! distinct attack and release shapes.
//!
//! State: single f32 (previous smoothed value) in `StateStore`.

use std::borrow::Cow;
use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const ENVELOPE_FOLLOWER_AR_TYPE_ID: &str = "node.envelope_follower_ar";

struct EnvelopeState {
    prev: f32,
}

impl NodeState for EnvelopeState {}

const ENVELOPE_INPUTS: [NodeInput; 4] = [
    NodePort {
        name: Cow::Borrowed("in"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("attack"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("release"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    // Reset on integer-edge changes. Drops the envelope to 0 so
    // the next emit re-attacks from zero. First observation arms
    // without firing so chain rebuilds don't cause spurious resets.
    // Matches the cross-primitive reset_trigger convention.
    NodePort {
        name: Cow::Borrowed("reset_trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const ENVELOPE_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const ENVELOPE_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("attack"),
        label: "Attack (s)",
        ty: ParamType::Float,
        default: ParamValue::Float(0.005),
        range: Some((0.0, 5.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("release"),
        label: "Release (s)",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 10.0)),
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct EnvelopeFollowerAr {
    type_id: EffectNodeType,
    /// Last observed `reset_trigger` integer. Matches the
    /// `array_feedback` / `node.feedback` convention — see
    /// `Smoothing::last_reset_trigger` for the rationale on storing
    /// this on the primitive struct rather than the StateStore
    /// entry (chain-rebuild robustness).
    last_reset_trigger: Option<i32>,
}

impl EnvelopeFollowerAr {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(ENVELOPE_FOLLOWER_AR_TYPE_ID),
            last_reset_trigger: None,
        }
    }
}

impl Default for EnvelopeFollowerAr {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for EnvelopeFollowerAr {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }

    fn inputs(&self) -> &[NodeInput] {
        &ENVELOPE_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &ENVELOPE_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &ENVELOPE_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let input_value = match ctx.inputs.scalar("in") {
            Some(ParamValue::Float(f)) => f,
            _ => return,
        };
        let attack = match ctx.inputs.scalar("attack") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => match ctx.params.get("attack") {
                Some(ParamValue::Float(f)) => f.max(0.0),
                _ => 0.005,
            },
        };
        let release = match ctx.inputs.scalar("release") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => match ctx.params.get("release") {
                Some(ParamValue::Float(f)) => f.max(0.0),
                _ => 0.5,
            },
        };
        let dt = ctx.time.delta.0 as f32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("EnvelopeFollowerAr::evaluate requires a StateStore");

        // Reset-on-trigger: on integer-edge changes, drop the
        // envelope to 0 so the next emit re-attacks from zero.
        // First observation arms without firing.
        let mut reset_now = false;
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = match self.last_reset_trigger {
                Some(prev_count) => current != prev_count,
                None => false,
            };
            self.last_reset_trigger = Some(current);
            reset_now = edge;
        }

        // First frame: initialise to the input so the envelope doesn't
        // bleed from 0 toward the first real measurement.
        // Reset edge: drop to 0 regardless of stored state.
        let prev = if reset_now {
            0.0
        } else {
            store
                .get::<EnvelopeState>(node_id, owner_key)
                .map(|s| s.prev)
                .unwrap_or(input_value)
        };

        // Pick time constant based on direction.
        let tau = if input_value > prev { attack } else { release };
        let alpha = if tau < 1e-6 {
            1.0
        } else {
            1.0 - (-dt / tau).exp()
        };
        let smoothed = prev + (input_value - prev) * alpha;

        store.insert(node_id, owner_key, EnvelopeState { prev: smoothed });

        ctx.outputs.set_scalar("out", ParamValue::Float(smoothed));
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: ENVELOPE_FOLLOWER_AR_TYPE_ID,
        create: || Box::new(EnvelopeFollowerAr::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Envelope Follower (A/R)",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_follower_ar_declares_four_inputs_and_one_output() {
        let node = EnvelopeFollowerAr::new();
        assert_eq!(node.inputs().len(), 4);
        assert_eq!(node.inputs()[0].name, "in");
        assert!(node.inputs()[0].required);
        assert_eq!(node.inputs()[1].name, "attack");
        assert!(!node.inputs()[1].required);
        assert_eq!(node.inputs()[2].name, "release");
        assert!(!node.inputs()[2].required);
        assert_eq!(node.inputs()[3].name, "reset_trigger");
        assert!(!node.inputs()[3].required);
        assert_eq!(node.outputs().len(), 1);
        assert_eq!(node.outputs()[0].name, "out");
    }

    #[test]
    fn envelope_follower_ar_has_attack_and_release_params() {
        let node = EnvelopeFollowerAr::new();
        let names: Vec<&str> = node.parameters().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["attack", "release"]);
    }

    #[test]
    fn envelope_follower_ar_type_id_is_node_prefixed() {
        let node = EnvelopeFollowerAr::new();
        assert_eq!(node.type_id().as_str(), "node.envelope_follower_ar");
    }
}
