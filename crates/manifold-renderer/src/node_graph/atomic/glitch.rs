//! [`Glitch`] — multi-mode digital corruption effect.
//!
//! The "irreducibly one shader" atomic effect example: chunk displacement,
//! channel shifts, and scanlines all packed into a single tight kernel.
//! Decomposing this into primitives would mean five dispatches where one
//! does today, so it stays atomic.

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use std::borrow::Cow;

pub const GLITCH_TYPE_ID: &str = "atomic.glitch";

pub const GLITCH_MODES: &[&str] = &[
    "BlockShift",
    "ChannelShift",
    "Scanlines",
    "DigitalNoise",
    "Combined",
];

const GLITCH_INPUTS: [NodeInput; 1] = [NodePort {
    name: Cow::Borrowed("source"),
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const GLITCH_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const GLITCH_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: Cow::Borrowed("intensity"),
        label: "Intensity",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("mode"),
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(4), // Combined
        range: None,
        enum_values: GLITCH_MODES,
    },
    ParamDef {
        name: Cow::Borrowed("shift_amount"),
        label: "Shift",
        ty: ParamType::Float,
        default: ParamValue::Float(0.05),
        range: Some((0.0, 0.5)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("block_size"),
        label: "Block Size",
        ty: ParamType::Float,
        default: ParamValue::Float(16.0),
        range: Some((1.0, 128.0)),
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct Glitch {
    type_id: EffectNodeType,
}

impl Glitch {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(GLITCH_TYPE_ID),
        }
    }
}

impl Default for Glitch {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Glitch {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Warp
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &GLITCH_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &GLITCH_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &GLITCH_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
