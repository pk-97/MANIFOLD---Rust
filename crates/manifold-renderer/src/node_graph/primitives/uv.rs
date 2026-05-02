//! UV-domain primitives: [`UVTransform`] (rewrite UVs), [`Sample`] (sample
//! with explicit per-pixel UVs).
//!
//! Both are the foundation for UV manipulation. `UVTransform` is a
//! UV-rewriting node in fusion terms; `Sample` is the explicit version
//! where the UV comes from another texture's RG channels.

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
// UVTransform — translate / scale / rotate / mirror the input.
//
// One node covers Mirror, QuadMirror, and Transform from the existing
// effect catalog (those become alias presets that pre-set `mode`).
// =====================================================================

pub const UV_TRANSFORM_TYPE_ID: &str = "primitive.uv_transform";

pub const UV_TRANSFORM_MODES: &[&str] = &[
    "Identity",
    "Mirror",
    "MirrorX",
    "MirrorY",
    "FlipY",
    "QuadMirror",
];

const UV_TRANSFORM_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const UV_TRANSFORM_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const UV_TRANSFORM_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: "translate",
        label: "Translate",
        ty: ParamType::Vec2,
        default: ParamValue::Vec2([0.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "scale",
        label: "Scale",
        ty: ParamType::Vec2,
        default: ParamValue::Vec2([1.0, 1.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "rotation",
        label: "Rotation",
        ty: ParamType::Float,
        // Radians; one full turn = 2π. Range left wide; 0..2π is convention.
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "mode",
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Identity
        range: None,
        enum_values: UV_TRANSFORM_MODES,
    },
];

#[derive(Debug)]
pub struct UVTransform {
    type_id: EffectNodeType,
}

impl UVTransform {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(UV_TRANSFORM_TYPE_ID),
        }
    }
}

impl Default for UVTransform {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for UVTransform {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &UV_TRANSFORM_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &UV_TRANSFORM_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &UV_TRANSFORM_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

// =====================================================================
// Sample — read source at UV taken from another texture's RG channels.
//
// `uv` input is a Texture2D where each pixel's R/G channels encode the
// (u, v) coordinate to sample from `source`. Useful for displacement,
// optical flow, lens distortion, custom warps.
// =====================================================================

pub const SAMPLE_TYPE_ID: &str = "primitive.sample";

pub const SAMPLE_FILTER_MODES: &[&str] = &["Nearest", "Linear"];
pub const SAMPLE_WRAP_MODES: &[&str] = &["Clamp", "Repeat", "Mirror"];

const SAMPLE_INPUTS: [NodeInput; 2] = [
    SOURCE_INPUT,
    NodePort {
        name: "uv",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
];

const SAMPLE_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const SAMPLE_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "filter",
        label: "Filter",
        ty: ParamType::Enum,
        default: ParamValue::Enum(1), // Linear
        range: None,
        enum_values: SAMPLE_FILTER_MODES,
    },
    ParamDef {
        name: "wrap",
        label: "Wrap",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Clamp
        range: None,
        enum_values: SAMPLE_WRAP_MODES,
    },
];

#[derive(Debug)]
pub struct Sample {
    type_id: EffectNodeType,
}

impl Sample {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(SAMPLE_TYPE_ID),
        }
    }
}

impl Default for Sample {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Sample {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &SAMPLE_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &SAMPLE_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &SAMPLE_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
