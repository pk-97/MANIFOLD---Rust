//! Filter primitives: [`Threshold`] (pixel-local), [`Blur`] (neighborhood),
//! [`MipChain`] (multi-pass).
//!
//! These three exercise the different fusion categories: Threshold is fully
//! fuseable, Blur breaks fusion with its input but accepts pixel-local
//! tail-fusion, and MipChain runs a series of passes regardless.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTextureFormat};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::render_target::RenderTarget;

const SOURCE_INPUT: NodeInput = NodePort {
    name: Cow::Borrowed("source"),
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Threshold — keep pixels above a luma cutoff (with optional softness).
// =====================================================================

pub const THRESHOLD_TYPE_ID: &str = "node.threshold";

const THRESHOLD_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const THRESHOLD_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const THRESHOLD_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("level"),
        label: "Threshold",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("softness"),
        label: "Softness",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ThresholdUniforms {
    level: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct Threshold {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl Threshold {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(THRESHOLD_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Threshold {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Threshold {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &THRESHOLD_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &THRESHOLD_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &THRESHOLD_PARAMS
    }
    // Hand-written node (no `primitive!` macro), so the fusion contract is
    // declared directly. The body is a verbatim port of threshold.wgsl's
    // response curve; the hand kernel stays authoritative for the standalone
    // dispatch.
    fn fusion_kind(&self) -> crate::node_graph::freeze::classify::FusionKind {
        crate::node_graph::freeze::classify::FusionKind::Pointwise
    }
    fn wgsl_body(&self) -> Option<&'static str> {
        Some(include_str!("shaders/threshold_body.wgsl"))
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let level = match ctx.params.get("level") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let softness = match ctx.params.get("softness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

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
                include_str!("shaders/threshold.wgsl"),
                "cs_main",
                "node.threshold",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ThresholdUniforms {
            level,
            softness,
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
            "node.threshold",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: THRESHOLD_TYPE_ID,
        create: || Box::new(Threshold::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Threshold", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// Blur — Gaussian/Box/Radial neighborhood blur.
// =====================================================================

pub const BLUR_TYPE_ID: &str = "node.blur";

pub const BLUR_MODES: &[&str] = &["Gaussian", "Box", "Radial"];

const BLUR_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const BLUR_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BLUR_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("radius"),
        label: "Radius",
        ty: ParamType::Float,
        default: ParamValue::Float(4.0),
        range: Some((0.0, 64.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("mode"),
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Gaussian
        range: None,
        enum_values: BLUR_MODES,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    radius: f32,
    mode: u32,
    direction: [f32; 2],
}

/// Format for the per-instance scratch texture used as the ping-pong
/// target between Blur's horizontal and vertical passes. Matches the
/// GRAPH_FORMAT used by graph-backed effects.
const BLUR_SCRATCH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct Blur {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    /// Per-instance ping-pong target. Allocated on first dispatch and
    /// reused across frames; recreated when the output dimensions
    /// change (rare — only on resolution change).
    scratch: Option<RenderTarget>,
}

impl Blur {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BLUR_TYPE_ID),
            pipeline: None,
            sampler: None,
            scratch: None,
        }
    }
}

impl Default for Blur {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Blur {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::BarrieredReduction)
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLUR_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLUR_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLUR_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let radius = match ctx.params.get("radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(i)) => *i,
            _ => 0,
        };

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
                include_str!("shaders/blur.wgsl"),
                "cs_main",
                "node.blur",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // (Re)allocate the scratch ping-pong texture if missing or sized wrong.
        let needs_scratch = match &self.scratch {
            Some(s) => s.width != width || s.height != height,
            None => true,
        };
        if needs_scratch {
            self.scratch = Some(RenderTarget::new(
                gpu.device,
                width,
                height,
                BLUR_SCRATCH_FORMAT,
                "node.blur scratch",
            ));
        }
        let scratch_tex = &self
            .scratch
            .as_ref()
            .expect("scratch allocated above")
            .texture;

        // Pass 1: horizontal — source → scratch.
        let uniforms_h = BlurUniforms {
            radius,
            mode,
            direction: [1.0, 0.0],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms_h),
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
                    texture: scratch_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.blur (H)",
        );

        // Pass 2: vertical — scratch → out.
        let uniforms_v = BlurUniforms {
            radius,
            mode,
            direction: [0.0, 1.0],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms_v),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: scratch_tex,
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
            "node.blur (V)",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: BLUR_TYPE_ID,
        create: || Box::new(Blur::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Blur", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// MipChain (node.mip_chain) was a no-op stub for an unbuilt multi-level
// downsample convention; removed when its only consumer (the legacy
// build_bloom composite) was retired. Bloom is now an explicit
// downsample → blur → mix graph in Bloom.json.
