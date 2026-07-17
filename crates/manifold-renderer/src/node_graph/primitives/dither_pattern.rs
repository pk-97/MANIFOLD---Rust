//! `node.dither_pattern` — ordered-dither / halftone threshold pattern
//! generator.
//!
//! Pure generator. Emits the per-pixel dither threshold in [0, 1]
//! (R = G = B = T, A = 1) for one of six algorithms — Bayer 8×8, Halftone
//! dots, Lines, CrossHatch, Blue Noise, Diamond — in screen space
//! (`pixel_pos = id + 0.5`, matching the legacy fused dither bit-for-bit).
//!
//! The reusable half of the old monolithic dither: pair with `node.dither`
//! (which consumes a pattern) for the full effect, or feed the threshold field
//! into any halftone / posterize / mask consumer. Because the pattern is now an
//! atom, the quantizer can be driven by ANY threshold texture — blue noise from
//! `node.noise`, a custom ramp, a voronoi field, etc.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherPatternUniforms {
    algorithm: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: DitherPattern,
    type_id: "node.dither_pattern",
    purpose: "Pure generator. Emits a per-pixel ordered-dither / halftone threshold in [0,1] (R=G=B=T, A=1) for one of six algorithms (Bayer 8x8, Halftone, Lines, CrossHatch, Blue Noise, Diamond), in screen space. The reusable pattern half of the dither effect — pair with node.dither (consumes a pattern) for the full effect, or feed any halftone / threshold consumer. The quantizer accepts ANY threshold texture, so you can also drive it from hash noise, a custom ramp, or a voronoi field.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("algorithm"),
            label: "Algorithm",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 5.0)),
            enum_values: &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Screen-space: pixel_pos = id + 0.5 (matches the legacy fused dither). Pattern density is intrinsic to the output pixel grid, so it stays size-coherent across render-scale changes. Output is constant in time. Pair: dither_pattern -> dither(in=source, pattern=dither_pattern.out, amount).",
    examples: ["preset.effect.dither"],
    picker: { label: "Dither Pattern", category: Atom },
    summary: "Generates the threshold grid that the Dither node uses to decide where pixels flip, with a choice of Bayer, halftone, and other patterns. Feed its output into Dither.",
    category: Stylize,
    role: Source,
    aliases: ["dither pattern", "bayer", "halftone", "threshold map"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/dither_pattern_body.wgsl"),
}

impl Primitive for DitherPattern {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let algorithm = match ctx.params.get("algorithm") {
            Some(ParamValue::Enum(v)) => (*v).min(5),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(5),
            _ => 0,
        };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.dither_pattern standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.dither_pattern",
            )
        });

        let uniforms = DitherPatternUniforms {
            algorithm,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.dither_pattern",
        );
    }
}
