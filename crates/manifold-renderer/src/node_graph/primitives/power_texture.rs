//! `node.power` — per-pixel `pow(max(input.rgb, 0), exponent)`.
//! Alpha pass-through.

use std::borrow::Cow;

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
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.power standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.power",
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
            "node.power",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn power_texture_declares_required_input_optional_exponent_and_one_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(PowerTexture::TYPE_ID, "node.power");
        assert_eq!(PowerTexture::INPUTS.len(), 2);
        assert_eq!(PowerTexture::INPUTS[0].name, "in");
        assert!(PowerTexture::INPUTS[0].required);
        assert_eq!(PowerTexture::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(PowerTexture::INPUTS[1].name, "exponent");
        assert!(!PowerTexture::INPUTS[1].required);
        assert_eq!(PowerTexture::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(PowerTexture::OUTPUTS.len(), 1);
        assert_eq!(PowerTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn power_texture_has_exponent_param() {
        let names: Vec<&str> = PowerTexture::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["exponent"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PowerTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.power");
    }
}
