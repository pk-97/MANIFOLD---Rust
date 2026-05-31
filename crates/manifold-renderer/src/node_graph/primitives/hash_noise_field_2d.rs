//! `node.hash_noise_field_2d` — uncorrelated per-pixel white-ish noise
//! via `wang_hash` on quantised UV. R channel in [0, 1]; GBA = (0, 0,
//! 1). Distinct from `node.simplex_noise_2d` (smooth correlated noise)
//! — this is high-frequency hash noise: film grain, dust, dither
//! sources, LIC ink for streamline visualisation.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HashNoiseUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    _pad0: f32,
}

crate::primitive! {
    name: HashNoiseField2D,
    type_id: "node.hash_noise_field_2d",
    purpose: "Pure generator. Uncorrelated per-pixel white-ish noise via wang_hash on quantised (uv*scale + offset). R = noise value in [0, 1]; GBA = (0, 0, 1). Distinct from `node.simplex_noise_2d` (smooth correlated) — this is high-frequency hash noise for film grain, dust, dither sources, and LIC ink (line-integral-convolution streamline visualisation).",
    inputs: {
        scale: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1024.0),
            range: Some((1.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_x",
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((0.0, 65536.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_y",
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((0.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Higher scale = finer noise (more cells across the canvas). Offsets shift the hash seed — animate them with an LFO for a flowing-dither look. Output is statistically uniform across [0, 1], not Gaussian; pair with `node.smoothstep_texture` or `node.tone_map` to shape distribution.",
    examples: [],
    picker: { label: "Hash Noise Field 2D", category: Atom },
    summary: "Sharp per-pixel random noise with no smoothing, the harsh static look. Good for grain, sparkle, and dissolve effects.",
    category: Noise,
    role: Source,
    aliases: ["hash noise", "white noise", "static", "random"],
}

impl Primitive for HashNoiseField2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let scale = read("scale", 1024.0);
        let offset_x = read("offset_x", 4096.0);
        let offset_y = read("offset_y", 4096.0);

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
                include_str!("shaders/hash_noise_field_2d.wgsl"),
                "cs_main",
                "node.hash_noise_field_2d",
            )
        });

        let uniforms = HashNoiseUniforms {
            scale,
            offset_x,
            offset_y,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.hash_noise_field_2d",
        );
    }
}
