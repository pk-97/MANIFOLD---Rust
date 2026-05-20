//! `node.sin_texture` — per-pixel `sin(input.rgb * freq + phase)`,
//! alpha pass-through.
//!
//! Per-pixel-math member of the Batch 5.5 procedural texture math
//! family. Chains downstream of field generators (uv_field,
//! distance_to_point, polar_field, noise) to create sinusoidal
//! patterns.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SinUniforms {
    freq: f32,
    phase: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: SinTexture,
    type_id: "node.sin_texture",
    purpose: "Per-pixel sin(input.rgb * freq + phase). Alpha passes through. Output range [-1, 1] (chain node.scale_offset_texture with a=0.5, b=0.5 to remap to [0, 1] for normal display). Used to turn scalar fields into oscillating patterns (rings around distance_to_point, sweeps around uv_field, etc.).",
    inputs: {
        in: Texture2D required,
        // Port-shadows-param for freq and phase: wired scalars
        // override the static params every frame. Lets generator
        // graphs drive phase from system.generator_input.time through
        // a Math primitive (multiply by a per-term constant).
        freq: ScalarF32 optional,
        phase: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "freq",
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(std::f32::consts::TAU),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "phase",
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    composition_notes: "Default freq = 2π so a [0, 1] input completes one full sine cycle. Animate by wiring `phase` from system.generator_input.time through a node.math (multiply by a per-term constant). Pair with node.distance_to_point for concentric rings, node.uv_field for stripes, node.polar_field's R channel for radial sectors.",
    examples: [],
    picker: { label: "Sin Texture", category: Atom },
}

impl Primitive for SinTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param: wired scalar (if present) overrides the
        // static param. Matches the convention Smoothing / Gain /
        // AffineTransform use.
        let freq = match ctx.inputs.scalar("freq") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("freq") {
                Some(ParamValue::Float(f)) => *f,
                _ => std::f32::consts::TAU,
            },
        };
        let phase = match ctx.inputs.scalar("phase") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("phase") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
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
                include_str!("shaders/sin_texture.wgsl"),
                "cs_main",
                "node.sin_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SinUniforms {
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
            "node.sin_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn sin_texture_declares_one_required_texture_input_and_two_optional_scalar_ports() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(SinTexture::TYPE_ID, "node.sin_texture");
        let ins = SinTexture::INPUTS;
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "freq");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[2].name, "phase");
        assert!(!ins[2].required);
        assert_eq!(ins[2].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(SinTexture::OUTPUTS.len(), 1);
        assert_eq!(SinTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn sin_texture_has_freq_and_phase_params() {
        let names: Vec<&str> = SinTexture::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["freq", "phase"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SinTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.sin_texture");
    }
}
