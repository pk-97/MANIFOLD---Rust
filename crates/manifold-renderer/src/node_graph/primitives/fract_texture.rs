//! `node.wrap` — per-pixel `fract(input.rgb * scale)`,
//! alpha pass-through.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FractUniforms {
    scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

manifold_node_engine::primitive! {
    name: FractTexture,
    type_id: "node.wrap",
    purpose: "Per-pixel fract(input.rgb * scale). Returns x - floor(x). Multiplying before fract is the classic 'tile a smooth field into N repeating stripes' trick — e.g. fract(uv.x * 10) → 10 vertical stripes from 0 to 1.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: name suggests a UV wrap but the body is `fract(input.rgb * scale)` — a per-pixel VALUE wrap, not a spatial UV remap
    depth_rule: Inherit,
    composition_notes: "Output range is [0, 1). Chain with node.uv_field for stripes, with node.distance_to_point for concentric rings (different aesthetic than sin_texture: sharp ramps instead of smooth oscillation). With node.voronoi_2d this turns F1 distances into per-cell intensity ramps.",
    examples: [],
    picker: { label: "Wrap", category: Atom },
    summary: "Keeps only the part after the decimal point, which wraps every value back into 0 to 1. Multiply the input first to tile or repeat a gradient.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["wrap", "fract texture", "fract", "repeat", "tile"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/fract_texture_body.wgsl"),
}

impl Primitive for FractTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.param_f32("scale", 1.0);

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

        let uniforms = FractUniforms {
            scale,
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
            "node.wrap",
        );
    }
}
