//! `node.power_texture` — per-pixel `pow(max(input.rgb, 0), exponent)`.
//! Alpha pass-through.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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
    type_id: "node.power_texture",
    purpose: "Per-pixel pow(max(input.rgb, 0), exponent). Alpha passes through. Sharpens or softens a [0, 1] field: exponent > 1 pushes mid-grays toward 0 (great for spiking voronoi F1 into star-points), exponent < 1 lifts darks (gamma-like brightening).",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "exponent",
            label: "Exponent",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 32.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Negative input is clamped to 0 before pow (pow of a negative base with a non-integer exponent is undefined). For signed fields, scale_offset_texture(0.5, 0.5) first, or pair with abs_texture. Star fields: voronoi_2d → fract_texture → power_texture(16) spikes the F1 distance into pinpoints.",
    examples: [],
    picker: { label: "Power Texture", category: Atom },
}

impl Primitive for PowerTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let exponent = match ctx.params.get("exponent") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
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
                include_str!("shaders/power_texture.wgsl"),
                "cs_main",
                "node.power_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = PowerUniforms {
            exponent,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
            "node.power_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn power_texture_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(PowerTexture::TYPE_ID, "node.power_texture");
        assert_eq!(PowerTexture::INPUTS.len(), 1);
        assert_eq!(PowerTexture::OUTPUTS.len(), 1);
        assert_eq!(PowerTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn power_texture_has_exponent_param() {
        let names: Vec<&str> = PowerTexture::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["exponent"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PowerTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.power_texture");
    }
}
