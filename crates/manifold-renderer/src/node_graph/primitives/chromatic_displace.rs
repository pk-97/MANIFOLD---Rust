//! `node.rgb_split` — 3-tap RGB sample of an input texture
//! displaced by a 2D vector field.
//!
//! R samples at `uv - velocity*amount/dims`, G at `uv`, B at
//! `uv + velocity*amount/dims`. Alpha follows the centre tap.
//!
//! Distinct from `node.chromatic_aberration` which splits RGB
//! radially around a centre. This is FLOW-driven: the displacement
//! direction is a per-pixel velocity, used for normal-map chromatic
//! splits (oily-fluid Oil Slick), signed-field chromatic trails,
//! anywhere the offset direction is data not symmetry.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticDisplaceUniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: ChromaticDisplace,
    type_id: "node.rgb_split",
    purpose: "3-tap RGB sample of `in` displaced by `velocity` (RG). R samples at `uv - velocity*amount/dims`, G at centre, B at `uv + …`. Alpha follows centre. Different from `node.chromatic_aberration` (radial split): this is FLOW-driven, the offset direction comes from a per-pixel velocity field. Used for normal-map chromatic splits in oily-fluid Oil Slick rendering, signed-field chromatic trails, anywhere displacement direction is data not symmetry.",
    inputs: {
        in: Texture2D required,
        velocity: Texture2D required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount (pixels)",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Velocity is read from RG; BA ignored. `amount` is in pixels — resolution-independent (shader divides by dims internally). Negative amount inverts the R/B sampling direction. Sample is bilinear; out-of-bounds uses the sampler's default clamp mode.",
    examples: [],
    picker: { label: "RGB Split", category: Atom },
    summary: "Pulls the red and blue channels apart along a direction you feed in, for a chromatic-aberration or glitchy colour-fringe look. The amount is in pixels and can go negative to swap which way they shift.",
    category: DistortAndWarp,
    role: Filter,
    aliases: ["rgb split", "chromatic displace", "chromatic aberration", "chroma shift", "color fringe"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/chromatic_displace_body.wgsl"),
    input_access: [Gather, Coincident],
}

impl Primitive for ChromaticDisplace {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.inputs.scalar("amount") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("amount") {
                Some(ParamValue::Float(f)) => *f,
                _ => 2.0,
            },
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(vel) = ctx.inputs.texture_2d("velocity") else {
            return;
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
            // Single-source: `in` is a Gather input (3-tap dependent sample). The
            // generated kernel's bindings match the set below (textures then
            // sampler). chromatic_displace.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.rgb_split standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rgb_split",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ChromaticDisplaceUniforms {
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
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: vel,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.rgb_split",
        );
    }
}
