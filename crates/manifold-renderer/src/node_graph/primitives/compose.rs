//! Composition primitives — combine two textures into one.
//!
//! [`Mix`] is a linear crossfade. [`Blend`] applies one of several blend
//! modes with an opacity. Both are pixel-local and fuseable.

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Mix — linear crossfade A → B.
// =====================================================================

pub const MIX_TYPE_ID: &str = "primitive.mix";

const MIX_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: "a",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: "b",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
];

const MIX_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const MIX_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "amount",
    label: "Amount",
    ty: ParamType::Float,
    default: ParamValue::Float(0.5),
    range: Some((0.0, 1.0)),
    enum_values: &[],
}];

#[derive(Debug)]
pub struct Mix {
    type_id: EffectNodeType,
}

impl Mix {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(MIX_TYPE_ID),
        }
    }
}

impl Default for Mix {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Mix {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &MIX_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &MIX_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &MIX_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

// =====================================================================
// Blend — composite two textures with a blend mode.
// =====================================================================

pub const BLEND_TYPE_ID: &str = "primitive.blend";

pub const BLEND_MODES: &[&str] = &[
    "Normal",
    "Add",
    "Multiply",
    "Screen",
    "Overlay",
    "Difference",
];

const BLEND_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: "base",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: "overlay",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
];

const BLEND_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BLEND_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "mode",
        label: "Blend Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Normal
        range: None,
        enum_values: BLEND_MODES,
    },
    ParamDef {
        name: "opacity",
        label: "Opacity",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct Blend {
    type_id: EffectNodeType,
}

impl Blend {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BLEND_TYPE_ID),
        }
    }
}

impl Default for Blend {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Blend {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLEND_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLEND_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLEND_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
