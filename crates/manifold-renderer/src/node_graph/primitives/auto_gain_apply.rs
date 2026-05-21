//! `node.auto_gain_apply` — multi-character gain coloration apply
//! pass. Bit-exact wrap of
//! `effects/shaders/auto_gain_apply.wgsl` via include_str.
//!
//! Five character modes (Clean / Warm / Film / Vivid / Grit) shape
//! the gain curve, with HDR retention, color push (saturation
//! shift proportional to gain delta), and parallel-compression
//! wet/dry mix.
//!
//! Pair upstream with `node.luminance` + `node.envelope_follower_ar`
//! to drive `gain` from frame brightness via attack/release dynamics
//! — the decomposed AutoGain pipeline.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::primitives::auto_gain::AUTO_GAIN_CHARACTERS;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AutoGainApplyUniforms {
    gain: f32,
    character: u32,
    color_push: f32,
    hdr_retention: f32,
    gain_delta: f32,
    amount: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: AutoGainApply,
    type_id: "node.auto_gain_apply",
    purpose: "Apply gain with character coloration to a Texture2D source. Five character modes (Clean / Warm / Film / Vivid / Grit) shape the gain curve via tube saturation, filmic shoulder, contrast / saturation boosts, or asymmetric clipping. HDR retention preserves above-1.0 energy independently. color_push adds saturation shift proportional to gain_delta. amount is the parallel-compression wet/dry mix.",
    inputs: {
        in: Texture2D required,
        gain: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "gain",
            label: "Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "character",
            label: "Character",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: AUTO_GAIN_CHARACTERS,
        },
        ParamDef {
            name: "color_push",
            label: "Color Push",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "hdr_retention",
            label: "HDR Retention",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "amount",
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "gain is port-shadowed — wire a node.luminance → node.envelope_follower_ar chain into it for the full AutoGain dynamic. gain_delta = gain - 1.0 is computed inside the primitive; you don't need to pass it. character options are luminance-energy-preserving (every mode rescales to match Clean's brightness so the compressor stays in control of overall level). amount = 0 bypasses the entire effect.",
    examples: [],
    picker: { label: "Auto Gain Apply", category: Atom },
}

impl Primitive for AutoGainApply {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gain = match ctx.inputs.scalar("gain") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("gain") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        let character = match ctx.params.get("character") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let color_push = match ctx.params.get("color_push") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let hdr_retention = match ctx.params.get("hdr_retention") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 0.0,
        };
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 1.0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../effects/shaders/auto_gain_apply.wgsl"),
                "cs_main",
                "node.auto_gain_apply",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = AutoGainApplyUniforms {
            gain,
            character,
            color_push,
            hdr_retention,
            gain_delta: gain - 1.0,
            amount,
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
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.auto_gain_apply",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn auto_gain_apply_declares_texture_in_gain_in_and_texture_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(AutoGainApply::TYPE_ID, "node.auto_gain_apply");
        assert_eq!(AutoGainApply::INPUTS.len(), 2);
        assert_eq!(AutoGainApply::INPUTS[0].name, "in");
        assert_eq!(AutoGainApply::INPUTS[0].ty, PortType::Texture2D);
        assert!(AutoGainApply::INPUTS[0].required);
        assert_eq!(AutoGainApply::INPUTS[1].name, "gain");
        assert!(!AutoGainApply::INPUTS[1].required);
        assert_eq!(AutoGainApply::OUTPUTS.len(), 1);
        assert_eq!(AutoGainApply::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn auto_gain_apply_has_five_character_options() {
        let p = AutoGainApply::PARAMS
            .iter()
            .find(|p| p.name == "character")
            .unwrap();
        assert_eq!(p.ty, ParamType::Enum);
        assert_eq!(p.enum_values.len(), 5);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = AutoGainApply::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.auto_gain_apply");
    }
}
