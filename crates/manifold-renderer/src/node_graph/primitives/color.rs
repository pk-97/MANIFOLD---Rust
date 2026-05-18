//! Color-domain primitives: [`Brightness`], [`ChannelMix`], [`ColorRamp`].
//!
//! All three are pixel-local: each output pixel depends only on the same
//! input pixel and parameters. They will fuse cleanly with each other and
//! with other pixel-local primitives once the fusion compiler lands.

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

const SOURCE_INPUT: NodeInput = NodePort {
    name: "source",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Brightness — RGB → grayscale via per-channel weights.
// =====================================================================

pub const BRIGHTNESS_TYPE_ID: &str = "node.brightness";

const BRIGHTNESS_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const BRIGHTNESS_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BRIGHTNESS_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "weights",
    label: "RGB Weights",
    ty: ParamType::Vec3,
    // Rec. 709 luma coefficients.
    default: ParamValue::Vec3([0.2126, 0.7152, 0.0722]),
    range: None,
    enum_values: &[],
}];

#[derive(Debug)]
pub struct Brightness {
    type_id: EffectNodeType,
}

impl Brightness {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BRIGHTNESS_TYPE_ID),
        }
    }
}

impl Default for Brightness {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Brightness {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &BRIGHTNESS_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BRIGHTNESS_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BRIGHTNESS_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: BRIGHTNESS_TYPE_ID,
        create: || Box::new(Brightness::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Brightness", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// ChannelMix — 4x4 RGBA transformation.
// =====================================================================

pub const CHANNEL_MIX_TYPE_ID: &str = "node.channel_mix";

const CHANNEL_MIX_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const CHANNEL_MIX_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const CHANNEL_MIX_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: "row0",
        label: "Row 0 (R)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([1.0, 0.0, 0.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row1",
        label: "Row 1 (G)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 1.0, 0.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row2",
        label: "Row 2 (B)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 0.0, 1.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row3",
        label: "Row 3 (A)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 0.0, 0.0, 1.0]),
        range: None,
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct ChannelMix {
    type_id: EffectNodeType,
}

impl ChannelMix {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(CHANNEL_MIX_TYPE_ID),
        }
    }
}

impl Default for ChannelMix {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for ChannelMix {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &CHANNEL_MIX_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &CHANNEL_MIX_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &CHANNEL_MIX_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: CHANNEL_MIX_TYPE_ID,
        create: || Box::new(ChannelMix::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Channel Mix", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// ColorRamp — luma → two-stop gradient lookup.
// =====================================================================

pub const COLOR_RAMP_TYPE_ID: &str = "node.color_ramp";

const COLOR_RAMP_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const COLOR_RAMP_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const COLOR_RAMP_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "color_a",
        label: "Color A",
        ty: ParamType::Color,
        default: ParamValue::Color([0.0, 0.0, 0.0, 1.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "color_b",
        label: "Color B",
        ty: ParamType::Color,
        default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
        range: None,
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct ColorRamp {
    type_id: EffectNodeType,
}

impl ColorRamp {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(COLOR_RAMP_TYPE_ID),
        }
    }
}

impl Default for ColorRamp {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for ColorRamp {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &COLOR_RAMP_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &COLOR_RAMP_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &COLOR_RAMP_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: COLOR_RAMP_TYPE_ID,
        create: || Box::new(ColorRamp::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Color Ramp", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}
