//! `node.trig_texture` — per-pixel sin/cos/tan of `(input.rgb * freq + phase)`.
//!
//! Replaces the old standalone `node.sin_texture` and `node.cos_texture`
//! with a single primitive that switches on a `mode` enum (Sin / Cos / Tan).
//! Same input/output/param shape regardless of mode — authors don't pick
//! the wrong primitive or need to swap nodes when iterating on a pattern.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TrigUniforms {
    freq: f32,
    phase: f32,
    mode: u32,
    _pad0: f32,
}

pub const TRIG_MODES: &[&str] = &["Sin", "Cos", "Tan"];

crate::primitive! {
    name: TrigTexture,
    type_id: "node.trig_texture",
    purpose: "Per-pixel trigonometric remap: out = trig_mode(input.rgb * freq + phase). Mode picks Sin / Cos / Tan; the rest of the wiring is identical so switching variants is one click. Tan output is clamped to ±32 to keep downstream shaders NaN/Inf-free.",
    inputs: {
        in: Texture2D required,
        // Port-shadows-param: wired scalars override the inline freq/phase.
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
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Sin
            range: Some((0.0, (TRIG_MODES.len() - 1) as f32)),
            enum_values: TRIG_MODES,
        },
    ],
    composition_notes: "Default freq = 2π so a [0, 1] input completes one full cycle. Sin and Cos output range is [-1, 1]; Tan is clamped to ±32. For Lissajous-style XY compositions, pair two trig_texture nodes (one Sin, one Cos) driven from the same field.",
    examples: [],
    picker: { label: "Trig Texture", category: Atom },
}

impl Primitive for TrigTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min((TRIG_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => (f.round() as u32).min((TRIG_MODES.len() - 1) as u32),
            _ => 0,
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
                include_str!("shaders/trig_texture.wgsl"),
                "cs_main",
                "node.trig_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TrigUniforms {
            freq,
            phase,
            mode,
            _pad0: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.trig_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn trig_texture_declares_required_in_and_optional_freq_phase() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(TrigTexture::TYPE_ID, "node.trig_texture");
        let ins = TrigTexture::INPUTS;
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
        assert_eq!(TrigTexture::OUTPUTS.len(), 1);
    }

    #[test]
    fn trig_texture_has_freq_phase_mode_params() {
        let names: Vec<&str> = TrigTexture::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["freq", "phase", "mode"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TrigTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.trig_texture");
    }
}
