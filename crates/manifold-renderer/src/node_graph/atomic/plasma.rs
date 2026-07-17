//! [`Plasma`] — classic plasma noise generator.
//!
//! A pure generator: no inputs, one Texture2D output. Stateless. Used as
//! the simplest atomic node example in V1 — the "hello world" of atomic
//! generators.

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use std::borrow::Cow;

pub const PLASMA_TYPE_ID: &str = "atomic.plasma";

const PLASMA_INPUTS: [NodeInput; 0] = [];

const PLASMA_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const PLASMA_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: Cow::Borrowed("speed"),
        label: "Speed",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 8.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("scale"),
        label: "Scale",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.1, 16.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("palette_a"),
        label: "Color A",
        ty: ParamType::Color,
        default: ParamValue::Color([0.0, 0.2, 0.6, 1.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("palette_b"),
        label: "Color B",
        ty: ParamType::Color,
        default: ParamValue::Color([1.0, 0.4, 0.0, 1.0]),
        range: None,
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct Plasma {
    type_id: EffectNodeType,
}

impl Plasma {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(PLASMA_TYPE_ID),
        }
    }
}

impl Default for Plasma {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Plasma {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::SourceHeight
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &PLASMA_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &PLASMA_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &PLASMA_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
