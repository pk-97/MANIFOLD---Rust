//! `node.kaleidoscope` — kaleidoscope coordinate generator. Emits the
//! per-pixel sample UV produced by folding the plane into N mirrored
//! wedges around a center. Pair with `node.remap` to sample any source at
//! the folded coordinates and `node.mix` to crossfade — the TD-style
//! `coordinate → remap → blend` shape that replaces the fused
//! `node.kaleidoscope` kernel.
//!
//! Output: R = folded_u, G = folded_v, B = 0, A = 1. The fold math is a
//! verbatim port of the legacy kaleidoscope, so `remap(Clamp) + mix(Lerp)`
//! reproduces it bit-for-bit. Reusable for any radial-symmetry warp
//! (mandala, mirror-wheel), not just kaleidoscope.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RadialFoldUvUniforms {
    segments: f32,
    cx: f32,
    cy: f32,
    _pad0: f32,
}

crate::primitive! {
    name: RadialFoldUv,
    type_id: "node.kaleidoscope",
    purpose: "Kaleidoscope coordinate generator: folds the plane into `segments` mirrored wedges around (cx, cy) and emits the per-pixel sample UV (R = folded_u, G = folded_v). Pair with node.remap (Clamp) to resample a source at the folded coordinates, then node.mix (Lerp) to crossfade — the TD coordinate → remap → blend shape that replaces the fused node.kaleidoscope kernel. Reusable for any radial-symmetry warp.",
    inputs: {
        segments: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("segments"),
            label: "Segments",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((2.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cx"),
            label: "Center X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cy"),
            label: "Center Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: zero-input fold-coordinate generator (same shape as centered_uv/mirror) consumed by a downstream node.remap — no depth origin of its own
    depth_rule: Terminal,
    composition_notes: "Verbatim port of the legacy node.kaleidoscope fold (raw atan2 angle, floor-based wedge index, alternating-wedge mirror, polar→cartesian reconstruct). `segments` floors to >= 2 and is port-shadowed so a counter / clip-trigger can drive the wedge count per retrigger. Output UVs are unclamped — let node.remap's Clamp wrap handle bounds (matches the legacy clamp-before-sample). Pair: source → radial_fold_uv → remap(source, uv_field) → mix(source, remapped, Lerp, amount).",
    examples: ["preset.effect.kaleidoscope"],
    picker: { label: "Kaleidoscope", category: Atom },
    summary: "Folds the image into a ring of mirrored wedges around a centre point. More segments give finer slices. It outputs warped coordinates, so pair it with Remap to apply them.",
    category: DistortAndWarp,
    role: Map,
    aliases: ["kaleidoscope", "radial fold", "mandala", "radial mirror", "wedges", "CC Kaleida"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/radial_fold_uv_body.wgsl"),
}

impl Primitive for RadialFoldUv {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let segments = ctx.scalar_or_param("segments", 6.0).max(2.0);
        let cx = ctx.scalar_or_param("cx", 0.5);
        let cy = ctx.scalar_or_param("cy", 0.5);

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.kaleidoscope standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.kaleidoscope",
            )
        });

        let uniforms = RadialFoldUvUniforms {
            segments,
            cx,
            cy,
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
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.kaleidoscope",
        );
    }
}
