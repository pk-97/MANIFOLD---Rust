//! `node.scale_offset_texture` — per-pixel affine remap
//! `a * x + b` on RGB (alpha pass-through).
//!
//! Companion to the per-pixel field generators in Batch 5.5 —
//! generators output [0, 1] for storage convenience; this primitive
//! is the affine inverse / re-range step.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScaleOffsetUniforms {
    scale: f32,
    offset: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: ScaleOffsetTexture,
    type_id: "node.scale_offset_texture",
    purpose: "Per-pixel affine remap `a * x + b` on RGB. Alpha pass-through. The general re-range primitive: use scale=2, offset=-1 to recover signed [-1, 1] noise from a [0, 1] generator; scale=0.5, offset=0.5 to compress signed sin/cos to [0, 1]; scale<0 to invert. Two-scalar version of node.gain + node.brightness fused.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-16.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset",
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-16.0, 16.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output = input * scale + offset, per RGB channel. Standard re-range recipes: (a=2, b=-1) maps [0, 1] → [-1, 1]; (a=0.5, b=0.5) maps [-1, 1] → [0, 1]; (a=-1, b=1) inverts; (a=1, b=0) is identity. Pair with node.sin_texture to compose ConcentricTunnel-style patterns.",
    examples: [],
    picker: { label: "Scale + Offset", category: Atom },
}

impl Primitive for ScaleOffsetTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let offset = match ctx.params.get("offset") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/scale_offset_texture.wgsl"),
                "cs_main",
                "node.scale_offset_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ScaleOffsetUniforms {
            scale,
            offset,
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
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.scale_offset_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scale_offset_texture_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(ScaleOffsetTexture::TYPE_ID, "node.scale_offset_texture");
        assert_eq!(ScaleOffsetTexture::INPUTS.len(), 1);
        assert_eq!(ScaleOffsetTexture::OUTPUTS.len(), 1);
        assert_eq!(ScaleOffsetTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn scale_offset_texture_has_scale_and_offset_params() {
        let names: Vec<&str> = ScaleOffsetTexture::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["scale", "offset"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScaleOffsetTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scale_offset_texture");
    }
}
