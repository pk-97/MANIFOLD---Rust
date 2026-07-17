//! `node.compressor_envelope` — visual-dynamics compressor envelope.
//!
//! One scalar in (a measured signal level, typically `node.luminance`
//! over the source) → one scalar out (a gain multiplier in the range
//! [0.1, 10.0]). Internally runs a log-domain, program-dependent
//! attack/release envelope follower with ratio compression toward a
//! user-set target level — same shape as a hardware audio compressor's
//! envelope path, applied to image brightness.
//!
//! Replaces the CPU envelope inside the legacy `AutoGainFX` bundle.
//! `node.envelope_follower_ar` is linear-domain symmetric A/R and
//! does NOT cover what this primitive needs (two EMAs for transient
//! detection, dynamic A/R, ratio compression to target). This is a
//! single curated CPU operation, same status as `node.smoothing` /
//! `node.one_euro_filter` / `node.envelope_follower_ar`.
//!
//! State (per `owner_key`) lives in `StateStore`:
//!   - `envelope_log`: fast log-luminance envelope ("the needle")
//!   - `long_term_log`: ~6 s long-term EMA for transient detection
//!   - `frame_count`: first-frame guard
//!
//! First frame returns gain = 1.0 and seeds both envelopes to the
//! first measured value — same first-frame behaviour as the legacy.

use std::borrow::Cow;

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const COMPRESSOR_ENVELOPE_TYPE_ID: &str = "node.compressor_envelope";

struct CompressorEnvelopeState {
    envelope_log: f32,
    long_term_log: f32,
    frame_count: u32,
}

impl NodeState for CompressorEnvelopeState {}

const COMPRESSOR_ENVELOPE_INPUTS: [NodeInput; 5] = [
    NodePort {
        name: Cow::Borrowed("in"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("ratio"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("sensitivity"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("target"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("reset_trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const COMPRESSOR_ENVELOPE_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const COMPRESSOR_ENVELOPE_PARAMS: [ParamDef; 3] = [
    ParamDef {
        name: Cow::Borrowed("ratio"),
        label: "Ratio",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("sensitivity"),
        label: "Sensitivity",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("target"),
        label: "Target",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct CompressorEnvelope {
    type_id: EffectNodeType,
    last_reset_trigger: Option<i32>,
}

impl CompressorEnvelope {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(COMPRESSOR_ENVELOPE_TYPE_ID),
            last_reset_trigger: None,
        }
    }
}

impl Default for CompressorEnvelope {
    fn default() -> Self {
        Self::new()
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

impl EffectNode for CompressorEnvelope {
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
        &COMPRESSOR_ENVELOPE_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &COMPRESSOR_ENVELOPE_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &COMPRESSOR_ENVELOPE_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let measured_lum = match ctx.inputs.scalar("in") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => return,
        };
        let ratio_param = ctx.scalar_or_param("ratio", 0.5).clamp(0.0, 1.0);
        let sensitivity_param = ctx.scalar_or_param("sensitivity", 0.5).clamp(0.0, 1.0);
        let target_param = ctx.scalar_or_param("target", 0.5).clamp(0.001, 1.0);
        let dt = ctx.time.delta.0 as f32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("CompressorEnvelope::evaluate requires a StateStore");

        let mut reset_now = false;
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = match self.last_reset_trigger {
                Some(prev) => current != prev,
                None => false,
            };
            self.last_reset_trigger = Some(current);
            reset_now = edge;
        }

        let epsilon = 0.0001_f32;
        let log_lum = (measured_lum + epsilon).log2();

        let mut state = if reset_now {
            CompressorEnvelopeState {
                envelope_log: 0.0,
                long_term_log: 0.0,
                frame_count: 0,
            }
        } else {
            store
                .get::<CompressorEnvelopeState>(node_id, owner_key)
                .map(|s| CompressorEnvelopeState {
                    envelope_log: s.envelope_log,
                    long_term_log: s.long_term_log,
                    frame_count: s.frame_count,
                })
                .unwrap_or(CompressorEnvelopeState {
                    envelope_log: 0.0,
                    long_term_log: 0.0,
                    frame_count: 0,
                })
        };

        // First frame: seed both envelopes to the measurement and
        // return unity gain. Matches the legacy first-frame guard.
        if state.frame_count == 0 {
            state.envelope_log = log_lum;
            state.long_term_log = log_lum;
            state.frame_count = 1;
            store.insert(node_id, owner_key, state);
            ctx.outputs.set_scalar("out", ParamValue::Float(1.0));
            return;
        }
        state.frame_count = state.frame_count.saturating_add(1);

        // Long-term EMA (~6 s) for program-dependent detection.
        let long_term_tc = 6.0_f32;
        let long_term_alpha = 1.0 - (-dt / long_term_tc).exp();
        state.long_term_log += (log_lum - state.long_term_log) * long_term_alpha;

        // Transient detection: ≥0.5 stops of deviation from the
        // long-term centre = "this is a hit, not the sustained level".
        let deviation = (log_lum - state.long_term_log).abs();
        let is_transient = deviation > 0.5;

        // Base A/R from Sensitivity. sensitivity=0 is lazy (250 ms
        // attack / 100 ms release — slow grab, transients survive,
        // breathes); sensitivity=1 is reactive (50 ms attack /
        // 1000 ms release — fast grab, holds reduction). Default
        // 0.5 is balanced. 60fps reference: 50 ms ≈ 3 frames,
        // 250 ms ≈ 15, 1000 ms ≈ 60, 100 ms ≈ 6.
        let base_attack = lerp(0.250, 0.050, sensitivity_param);
        let base_release = lerp(0.100, 1.000, sensitivity_param);
        let attack = if is_transient {
            base_attack * 2.0
        } else {
            base_attack
        };
        let release = if is_transient {
            base_release * 0.5
        } else {
            base_release
        };

        let time_constant = if log_lum > state.envelope_log {
            attack
        } else {
            release
        };
        let alpha = 1.0 - (-dt / time_constant.max(0.001)).exp();
        state.envelope_log += (log_lum - state.envelope_log) * alpha;

        // Compress deviation from target_log by ratio (1:1 .. 10:1).
        let target_log = target_param.log2();
        let env_deviation = state.envelope_log - target_log;
        let ratio = 1.0 + ratio_param * 9.0;
        let compressed_deviation = env_deviation / ratio;
        let desired_log = target_log + compressed_deviation;
        let gain = 2.0_f32.powf(desired_log - state.envelope_log);
        let gain = gain.clamp(0.1, 10.0);

        store.insert(node_id, owner_key, state);
        ctx.outputs.set_scalar("out", ParamValue::Float(gain));
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: COMPRESSOR_ENVELOPE_TYPE_ID,
        create: || Box::new(CompressorEnvelope::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Compressor Envelope",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_id_is_node_prefixed() {
        let node = CompressorEnvelope::new();
        assert_eq!(node.type_id().as_str(), "node.compressor_envelope");
    }

    #[test]
    fn declares_in_three_modulation_ports_reset_and_out() {
        let node = CompressorEnvelope::new();
        let ins: Vec<&str> = node.inputs().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(ins, vec!["in", "ratio", "sensitivity", "target", "reset_trigger"]);
        assert!(node.inputs()[0].required);
        for i in 1..5 {
            assert!(!node.inputs()[i].required);
        }
        let outs: Vec<&str> = node.outputs().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(outs, vec!["out"]);
    }

    #[test]
    fn has_ratio_sensitivity_target_params() {
        let node = CompressorEnvelope::new();
        let names: Vec<&str> = node.parameters().iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["ratio", "sensitivity", "target"]);
    }
}
