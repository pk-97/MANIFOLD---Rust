//! `node.simplex_field_2d` — 3D Perlin-style simplex noise as a
//! signed scalar field.
//!
//! Output is the raw signed noise value (approximately [-1, +1])
//! written to the R channel; GBA = (0, 0, 1). Distinct from
//! `node.noise` which is 2D and remaps to [0, 1] for
//! visual use.
//!
//! Use this when noise drives downstream math — fluid-sim seed
//! patterns, color injection, displacement fields. The Z axis lets a
//! static node sample an evolving noise field: animate `z` over time
//! for in-place turbulence (vs `simplex_noise_2d`'s offset_x/y which
//! pans through a frozen field). Independent seed offsets per call
//! (different `z` values, or large fixed offsets in x/y) give
//! statistically uncorrelated channels — the FBM and color/velocity
//! independence of the oily-fluid family rely on this.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SIMPLEX_FIELD_OUTPUT_CHANNELS: &[&str] = &["R", "G", "B", "A"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimplexFieldUniforms {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
    z: f32,
    output_channel: u32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: SimplexField2D,
    type_id: "node.simplex_field_2d",
    purpose: "Pure generator. 3D Perlin-style simplex noise sampled at `(uv * scale + offset, z)`. Outputs the SIGNED noise value in approximately [-1, +1] to the R channel (GBA = 0, 0, 1). The Z axis is what makes a static node produce an evolving field — animate `z` for turbulent shimmer in place; pan `offset_x` / `offset_y` for directional flow through a frozen field. Use this when noise drives downstream math (displacement, color injection, fluid-sim seeding); use `node.noise` when you want a [0, 1] visual texture.",
    inputs: {
        scale_x: ScalarF32 optional,
        scale_y: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
        z: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale_x"),
            label: "Scale X",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_y"),
            label: "Scale Y",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z"),
            label: "Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("output_channel"),
            label: "Output Channel",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: SIMPLEX_FIELD_OUTPUT_CHANNELS,
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Aspect correction belongs OUTSIDE this primitive: wire `system.generator_input.aspect → math.multiply(value=N) → scale_x` to keep the noise isotropic on non-square canvases (this is the convention oily-fluid uses for its color/velocity seeding). Multi-octave FBM is built externally: chain N instances at scales (S, S*lacunarity, S*lacunarity²…) into N gain nodes (weights 1, persistence, persistence²…) summed via `node.compose` — packing FBM into one primitive locks the octave count; composing it stays editable. Independent channels: assign distinct `z` values per node (a large numeric gap like 5.0 between channels is enough).",
    examples: [],
    picker: { label: "Simplex Field 2D", category: Atom },
    summary: "Signed simplex noise output as a field, used to drive flows and displacements rather than shown directly.",
    category: Noise,
    role: Source,
    aliases: ["simplex field", "noise field", "flow"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/simplex_field_2d_body.wgsl"),
}

impl Primitive for SimplexField2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale_x = match ctx.inputs.scalar("scale_x") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("scale_x") {
                Some(ParamValue::Float(f)) => *f,
                _ => 3.0,
            },
        };
        let scale_y = match ctx.inputs.scalar("scale_y") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("scale_y") {
                Some(ParamValue::Float(f)) => *f,
                _ => 3.0,
            },
        };
        let offset_x = match ctx.inputs.scalar("offset_x") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("offset_x") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let offset_y = match ctx.inputs.scalar("offset_y") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("offset_y") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let z = match ctx.inputs.scalar("z") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("z") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let output_channel = match ctx.params.get("output_channel") {
            Some(ParamValue::Enum(v)) => (*v).min(3),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(3),
            _ => 0,
        };

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.simplex_field_2d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.simplex_field_2d",
            )
        });

        let uniforms = SimplexFieldUniforms {
            scale_x,
            scale_y,
            offset_x,
            offset_y,
            z,
            output_channel,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.simplex_field_2d",
        );
    }
}
