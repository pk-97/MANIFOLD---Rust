//! `node.power` — per-pixel `pow(max(input.rgb, 0), exponent)`.
//! Alpha pass-through.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PowerUniforms {
    exponent: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: PowerTexture,
    type_id: "node.power",
    purpose: "Per-pixel pow(max(input.rgb, 0), exponent). Alpha passes through. Sharpens or softens a [0, 1] field: exponent > 1 pushes mid-grays toward 0 (great for spiking voronoi F1 into star-points), exponent < 1 lifts darks (gamma-like brightening).",
    inputs: {
        in: Texture2D required,
        // Port-shadow on exponent so a slider / LFO / per-cell-hash
        // chain can animate spike sharpness (StarField uses this to
        // give the user a "Star Size" knob driving exponent).
        exponent: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("exponent"),
            label: "Exponent",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 32.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Negative input is clamped to 0 before pow (pow of a negative base with a non-integer exponent is undefined). For signed fields, scale_offset_texture(0.5, 0.5) first, or pair with abs_texture. Star fields: voronoi_2d → fract_texture → power_texture(16) spikes the F1 distance into pinpoints.",
    examples: [],
    picker: { label: "Power", category: Atom },
    summary: "Raises each value to a power, which sharpens or softens a 0-to-1 field. Above 1 pushes toward black, below 1 lifts the midtones.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["power", "power texture", "pow", "exponent", "gamma"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/power_texture_body.wgsl"),
}

impl Primitive for PowerTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let exponent = ctx.scalar_or_param("exponent", 2.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = PowerUniforms {
            exponent,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.power",
        );
    }
}
