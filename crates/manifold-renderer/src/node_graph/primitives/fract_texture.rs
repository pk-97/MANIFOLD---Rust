//! `node.wrap` — per-pixel `fract(input.rgb * scale)`,
//! alpha pass-through.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FractUniforms {
    scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
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
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
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
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.wrap standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.wrap",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FractUniforms {
            scale,
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
            "node.wrap",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fract_texture_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(FractTexture::TYPE_ID, "node.wrap");
        assert_eq!(FractTexture::INPUTS.len(), 1);
        assert_eq!(FractTexture::OUTPUTS.len(), 1);
        assert_eq!(FractTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn fract_texture_has_scale_param() {
        let names: Vec<&str> = FractTexture::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["scale"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FractTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wrap");
    }
}
