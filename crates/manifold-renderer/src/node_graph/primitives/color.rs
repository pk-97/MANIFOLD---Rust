//! Color-domain primitives: [`Luminance`], [`ColorMatrix`], [`GradientMap`].
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
// Luminance — RGB → grayscale via per-channel weights.
// =====================================================================

pub const LUMINANCE_TYPE_ID: &str = "primitive.luminance";

const LUMINANCE_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const LUMINANCE_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const LUMINANCE_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "weights",
    label: "RGB Weights",
    ty: ParamType::Vec3,
    // Rec. 709 luma coefficients.
    default: ParamValue::Vec3([0.2126, 0.7152, 0.0722]),
    range: None,
    enum_values: &[],
}];

#[derive(Debug)]
pub struct Luminance {
    type_id: EffectNodeType,
}

impl Luminance {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(LUMINANCE_TYPE_ID),
        }
    }
}

impl Default for Luminance {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Luminance {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &LUMINANCE_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &LUMINANCE_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &LUMINANCE_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

// =====================================================================
// ColorMatrix — 4x4 RGBA transformation.
// =====================================================================

pub const COLOR_MATRIX_TYPE_ID: &str = "primitive.color_matrix";

const COLOR_MATRIX_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const COLOR_MATRIX_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const COLOR_MATRIX_PARAMS: [ParamDef; 4] = [
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
pub struct ColorMatrix {
    type_id: EffectNodeType,
}

impl ColorMatrix {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(COLOR_MATRIX_TYPE_ID),
        }
    }
}

impl Default for ColorMatrix {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for ColorMatrix {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &COLOR_MATRIX_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &COLOR_MATRIX_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &COLOR_MATRIX_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

// =====================================================================
// GradientMap — luma → two-stop gradient lookup.
// =====================================================================

pub const GRADIENT_MAP_TYPE_ID: &str = "primitive.gradient_map";

const GRADIENT_MAP_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const GRADIENT_MAP_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const GRADIENT_MAP_PARAMS: [ParamDef; 2] = [
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
pub struct GradientMap {
    type_id: EffectNodeType,
}

impl GradientMap {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(GRADIENT_MAP_TYPE_ID),
        }
    }
}

impl Default for GradientMap {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for GradientMap {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &GRADIENT_MAP_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &GRADIENT_MAP_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &GRADIENT_MAP_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
