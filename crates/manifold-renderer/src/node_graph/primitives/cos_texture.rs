//! `node.cos_texture` — per-pixel `cos(input.rgb * freq + phase)`,
//! alpha pass-through. Sibling to `node.sin_texture`.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CosUniforms {
    freq: f32,
    phase: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: CosTexture,
    type_id: "node.cos_texture",
    purpose: "Per-pixel cos(input.rgb * freq + phase). Alpha passes through. Output range [-1, 1]. Sibling of node.sin_texture; offered as its own primitive so authors don't have to compute a π/2 phase shift to switch between them.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "freq",
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(6.28318530717958647692),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "phase",
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-6.28318530717958647692, 6.28318530717958647692)),
            enum_values: &[],
        },
    ],
    composition_notes: "Identical wiring to node.sin_texture — same freq / phase conventions, same input/output shape. Pair with node.sin_texture (driven from the same field) for Lissajous-style XY compositions.",
    examples: [],
    picker: { label: "Cos Texture", category: Atom },
}

impl Primitive for CosTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let freq = match ctx.params.get("freq") {
            Some(ParamValue::Float(f)) => *f,
            _ => std::f32::consts::TAU,
        };
        let phase = match ctx.params.get("phase") {
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
                include_str!("shaders/cos_texture.wgsl"),
                "cs_main",
                "node.cos_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = CosUniforms {
            freq,
            phase,
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
            "node.cos_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn cos_texture_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(CosTexture::TYPE_ID, "node.cos_texture");
        assert_eq!(CosTexture::INPUTS.len(), 1);
        assert_eq!(CosTexture::OUTPUTS.len(), 1);
        assert_eq!(CosTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn cos_texture_has_freq_and_phase_params() {
        let names: Vec<&str> = CosTexture::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["freq", "phase"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CosTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.cos_texture");
    }
}
