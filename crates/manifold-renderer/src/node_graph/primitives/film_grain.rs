//! `node.film_grain` — multiplicative white-noise grain. Each pixel is
//! darkened by a per-pixel hash so bright areas pick up paper-like
//! texture while black stays black: `out.rgb = src.rgb * (1 - amount *
//! (1 - white_noise(pixel)))`. Alpha passes through.
//!
//! The grain pass of Watercolor, extracted as a reusable atom — a quick
//! film/paper grain over any source. `amount` port-shadows the param.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilmGrainUniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: FilmGrain,
    type_id: "node.film_grain",
    purpose: "Multiplicative white-noise grain: out.rgb = src.rgb * (1 - amount * (1 - white_noise(pixel))). Black stays black, bright areas pick up paper-like texture — simulates paper absorbing paint unevenly. Alpha passes through. The grain pass of Watercolor as a reusable atom; `amount` port-shadows the param.",
    inputs: {
        in: Texture2D required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "white_noise(coord) = fract(sin(dot(coord, vec2(12.9898, 78.233))) * 43758.5453), coord in pixels. Static (no time) — for animated grain, wire a time-varying scalar into amount or chain a per-frame hash offset. Wire wins over param.",
    examples: ["preset.effect.watercolor"],
    picker: { label: "Film Grain", category: Atom },
    summary: "Lays fine film-style grain over the image, heavier in the bright areas like real photographic stock. Dial the amount for a subtle texture or heavy noise.",
    category: Stylize,
    role: Filter,
    aliases: ["film grain", "grain", "noise", "16mm", "Add Grain"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/film_grain_body.wgsl"),
}

impl Primitive for FilmGrain {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.inputs.scalar("amount") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("amount") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.15,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: standalone kernel generated from the same
            // `wgsl_body` the fusion codegen chains. Positional — its body reads
            // the ambient uv/dims to recover pixel = uv*dims. film_grain.wgsl is
            // retained as the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.film_grain standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.film_grain",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FilmGrainUniforms {
            amount,
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
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.film_grain",
        );
    }
}
