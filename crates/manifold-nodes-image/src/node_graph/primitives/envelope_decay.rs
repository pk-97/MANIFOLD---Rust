//! `node.envelope_decay` — exponential one-shot decay triggered by
//! integer-edge changes on a scalar input.
//!
//! On each new `trigger` value (any integer change vs. the previous
//! frame), the envelope snaps to `1.0`. Between triggers it decays
//! frame-rate-independently: `env *= exp(-decay_rate * dt)`.
//!
//! Sole purpose primitive — driving the four scalar modes of
//! FluidSim2D's clip-trigger state machine (noise burst, rotation
//! flip, slope flip, inject phase) and any other "pulse on clip,
//! decay back to zero" control surface. The reusable atom version of
//! the legacy `clip_trigger_envelope` field in `FluidSimCore`.
//!
//! State: `last_trigger: i32` + `envelope: f32` in [`StateStore`].
//! Cleared on seek / pause so a re-entered clip starts at zero.
//!
//! [`StateStore`]: crate::node_graph::StateStore

use manifold_node_engine::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{
    NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
};
use manifold_node_engine::state_store::NodeState;
use std::borrow::Cow;

pub const ENVELOPE_DECAY_TYPE_ID: &str = "node.envelope_decay";

struct DecayState {
    last_trigger: i32,
    envelope: f32,
}

impl NodeState for DecayState {}

const ENVELOPE_DECAY_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: Cow::Borrowed("trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    // Port-shadows-param: lets the decay rate be driven from a slider
    // or another node (e.g. tying the decay rate to BPM for tempo-
    // synced strobe envelopes).
    NodePort {
        name: Cow::Borrowed("decay_rate"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const ENVELOPE_DECAY_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const ENVELOPE_DECAY_PARAMS: [ParamDef; 1] = [ParamDef {
    name: Cow::Borrowed("decay_rate"),
    label: "Decay Rate",
    ty: ParamType::Float,
    default: ParamValue::Float(12.0),
    range: Some((0.0, 60.0)),
    enum_values: &[],
}];

#[derive(Debug)]
pub struct EnvelopeDecay {
    type_id: EffectNodeType,
}

impl EnvelopeDecay {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(ENVELOPE_DECAY_TYPE_ID),
        }
    }
}

impl Default for EnvelopeDecay {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for EnvelopeDecay {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<manifold_node_engine::freeze::classify::BoundaryReason> {
        Some(manifold_node_engine::freeze::classify::BoundaryReason::NonGpu)
    }

    fn inputs(&self) -> &[NodeInput] {
        &ENVELOPE_DECAY_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &ENVELOPE_DECAY_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &ENVELOPE_DECAY_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let trigger_value = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => return,
        };
        let decay_rate = match ctx.inputs.scalar("decay_rate") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => match ctx.params.get("decay_rate") {
                Some(ParamValue::Float(f)) => f.max(0.0),
                _ => 12.0,
            },
        };
        let dt = ctx.time.delta.0 as f32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("EnvelopeDecay::evaluate requires a StateStore");

        // Initial state: no prior trigger (-1 sentinel matches the
        // legacy FluidSimCore `last_trigger_count` semantics — the
        // first frame after seek doesn't fire even if `trigger > 0`).
        let (last_trigger, prev_env) = store
            .get::<DecayState>(node_id, owner_key)
            .map(|s| (s.last_trigger, s.envelope))
            .unwrap_or((-1, 0.0));

        let mut envelope = prev_env;

        if trigger_value != last_trigger {
            // First observation (last_trigger == -1) arms the state
            // machine without pulsing. Matches the
            // `self.last_trigger_count >= 0` guard in fluid_sim_core.
            if last_trigger >= 0 {
                envelope = 1.0;
            }
        }

        // Frame-rate-independent exponential decay. Threshold mirrors
        // legacy's `clip_trigger_envelope > 0.001` clamp.
        if envelope > 0.001 {
            envelope *= (-decay_rate * dt).exp();
        } else {
            envelope = 0.0;
        }

        store.insert(
            node_id,
            owner_key,
            DecayState {
                last_trigger: trigger_value,
                envelope,
            },
        );

        ctx.outputs.set_scalar("out", ParamValue::Float(envelope));
    }
}

inventory::submit! {
    manifold_node_engine::persistence::PrimitiveFactory {
        type_id: ENVELOPE_DECAY_TYPE_ID,
        create: || Box::new(EnvelopeDecay::new()),
        picker: Some(manifold_node_engine::palette::PickerInfo {
            label: "Envelope Decay",
            category: manifold_node_engine::palette::PaletteCategory::Driver,
        }),
    }
}

