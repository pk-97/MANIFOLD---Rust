//! UV-domain primitives: [`Transform`] (rewrite UVs), [`Sample`] (sample
//! with explicit per-pixel UVs).
//!
//! Both are the foundation for UV manipulation. `Transform` is a
//! UV-rewriting node in fusion terms; `Sample` is the explicit version
//! where the UV comes from another texture's RG channels.

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

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
// Transform — translate / scale / rotate / mirror the input.
//
// One node covers Mirror, QuadMirror, and Transform from the existing
// effect catalog (those become alias presets that pre-set `mode`).
// =====================================================================

pub const TRANSFORM_TYPE_ID: &str = "node.transform";

pub const TRANSFORM_MODES: &[&str] = &[
    "Identity",
    "Mirror",
    "MirrorX",
    "MirrorY",
    "FlipY",
    "QuadMirror",
    // Fold modes — the legacy Mirror effect's kaleidoscope behavior:
    // each axis is mirrored across its center, so half the source is
    // visible and the other half is its mirror image.
    "FoldX",
    "FoldY",
    "FoldBoth",
];

const TRANSFORM_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const TRANSFORM_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const TRANSFORM_PARAMS: [ParamDef; 4] = [
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
        enum_values: TRANSFORM_MODES,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UVTransformUniforms {
    translate: [f32; 2],
    scale: [f32; 2],
    rotation: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

pub struct Transform {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl Transform {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(TRANSFORM_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Transform {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &TRANSFORM_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &TRANSFORM_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &TRANSFORM_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let translate = match ctx.params.get("translate") {
            Some(ParamValue::Vec2(v)) => *v,
            _ => [0.0, 0.0],
        };
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Vec2(v)) => *v,
            _ => [1.0, 1.0],
        };
        let rotation = match ctx.params.get("rotation") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(i)) => *i,
            _ => 0,
        };

        // Resolve textures up-front; lifetimes survive the encoder borrow.
        let Some(source) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/uv_transform.wgsl"),
                "cs_main",
                "node.transform",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = UVTransformUniforms {
            translate,
            scale,
            rotation,
            mode,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.transform",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: TRANSFORM_TYPE_ID,
        create: || Box::new(Transform::new()),
    }
}

// =====================================================================
// Sample — read source at UV taken from another texture's RG channels.
//
// `uv` input is a Texture2D where each pixel's R/G channels encode the
// (u, v) coordinate to sample from `source`. Useful for displacement,
// optical flow, lens distortion, custom warps.
// =====================================================================

pub const SAMPLE_TYPE_ID: &str = "node.sample";

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

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: SAMPLE_TYPE_ID,
        create: || Box::new(Sample::new()),
    }
}
