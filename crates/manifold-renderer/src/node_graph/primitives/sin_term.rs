//! `node.sine_wave` — fused linear-projection + sin term.
//!
//! `out = sin((a * field.r + b * field.g + c) * freq * freq_scale + time * time_scale)`
//!
//! The natural shape for one term of any sum-of-sines pattern (Plasma's
//! five summed sines, moiré, parametric standing waves). The `field`
//! input is any Texture2D — typically a coordinate texture from
//! `node.centered_uv` (R = x, G = y) for linear projections, or a
//! pre-computed scalar field from `node.distance_to_point` (R=G=B=value)
//! for non-linear projections where the defaults a=1, b=0, c=0 read
//! the broadcast R channel directly.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SinTermUniforms {
    a: f32,
    b: f32,
    c: f32,
    freq: f32,
    freq_scale: f32,
    time: f32,
    time_scale: f32,
    _pad0: f32,
}

crate::primitive! {
    name: SinTerm,
    type_id: "node.sine_wave",
    purpose: "Fused linear-projection + sin term: out = sin((a*field.r + b*field.g + c) * freq * freq_scale + time * time_scale). One node per term of any sum-of-sines pattern (Plasma, moiré, standing waves). For a linear projection of UV channels set (a, b) to pick the projection; for a pre-computed scalar field (distance, noise) leave defaults (a=1, b=0, c=0) so it reads the broadcast R channel.",
    inputs: {
        // Field texture — coordinate texture (R = x, G = y) for linear
        // projections, or scalar field (R=G=B=value) for non-linear.
        field: Texture2D required,
        // Port-shadowable shared scalars — drive freq from one
        // upstream value and time from system.generator_input.time;
        // each instance contributes only its per-term scales as params.
        freq: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("a"),
            label: "X Coefficient",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("b"),
            label: "Y Coefficient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("c"),
            label: "Constant Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("freq"),
            label: "Frequency (base)",
            ty: ParamType::Float,
            default: ParamValue::Float(std::f32::consts::TAU),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("freq_scale"),
            label: "Frequency Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time (base)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time_scale"),
            label: "Time Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Pick (a, b, c) to choose the field projection: (1,0,0)=along X, (0,1,0)=along Y, (1,1,0)=diagonal X+Y. Pair with `node.rotate_coordinates` upstream for rotated projections — feed the rotated UV in and keep (a, b) = (1, 0). Wire `freq` from a shared value node and `time` from system.generator_input.time so all five terms in a Plasma-style sum stay phase-coherent.",
    examples: [],
    picker: { label: "Sine Wave (projected)", category: Atom },
    summary: "Mixes a coordinate field into a moving sine wave in one step, the core ingredient of plasma and interference patterns.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["sine wave", "sin term", "plasma", "wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sin_term_body.wgsl"),
}

impl Primitive for SinTerm {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let a = match ctx.params.get("a") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let b = match ctx.params.get("b") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let c = match ctx.params.get("c") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let freq = ctx.scalar_or_param("freq", std::f32::consts::TAU);
        let freq_scale = match ctx.params.get("freq_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let time = ctx.scalar_or_param("time", 0.0);
        let time_scale = match ctx.params.get("time_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("field") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.sine_wave standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.sine_wave",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SinTermUniforms {
            a,
            b,
            c,
            freq,
            freq_scale,
            time,
            time_scale,
            _pad0: 0.0,
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
            "node.sine_wave",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn sin_term_declares_field_required_freq_time_optional() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(SinTerm::TYPE_ID, "node.sine_wave");
        let ins = SinTerm::INPUTS;
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "field");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "freq");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[2].name, "time");
        assert!(!ins[2].required);
        assert_eq!(ins[2].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(SinTerm::OUTPUTS.len(), 1);
    }

    #[test]
    fn sin_term_has_seven_params() {
        let names: Vec<&str> = SinTerm::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["a", "b", "c", "freq", "freq_scale", "time", "time_scale"],
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SinTerm::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.sine_wave");
    }
}

