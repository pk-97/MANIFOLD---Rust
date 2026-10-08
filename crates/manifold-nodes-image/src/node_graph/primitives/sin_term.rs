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
use manifold_gpu::GpuSamplerDesc;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

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

manifold_node_engine::primitive! {
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
        let a = ctx.param_f32("a", 1.0);

        let b = ctx.param_f32("b", 0.0);

        let c = ctx.param_f32("c", 0.0);

        let freq = ctx.scalar_or_param("freq", std::f32::consts::TAU);
        let freq_scale = ctx.param_f32("freq_scale", 1.0);

        let time = ctx.scalar_or_param("time", 0.0);
        let time_scale = ctx.param_f32("time_scale", 1.0);

        let Some(in_tex) = ctx.inputs.texture_2d("field") else {
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

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.sine_wave",
        );
    }
}

