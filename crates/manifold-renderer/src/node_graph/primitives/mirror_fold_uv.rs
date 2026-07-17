//! `node.mirror` — mirror / fold coordinate generator. Emits the
//! per-pixel sample UV produced by the legacy `TransformFX` mirror/fold
//! mode table (axis flips + kaleidoscope-style folds). Pair with
//! `node.remap` to resample any source at the rewritten coordinates and
//! `node.mix` to crossfade — the TD-style `coordinate → remap → blend`
//! shape that replaces the fused `TransformFX` mirror modes.
//!
//! Output: R = folded_u, G = folded_v, B = 0, A = 1. The fold math is a
//! verbatim port of `uv_transform.wgsl`'s mirror pass, so
//! `remap(Clamp) + mix(Lerp)` reproduces the legacy Mirror effect
//! bit-for-bit. The affine half of `TransformFX` (translate / scale /
//! rotate) is intentionally left to `node.transform`.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Mirror/fold modes, indexed by the `mode` enum. Mirrors the legacy
/// `TransformFX` table (the affine-only `Identity` plus eight
/// mirror/fold variants).
pub const MIRROR_FOLD_MODES: &[&str] = &[
    "Identity",
    "Mirror",
    "MirrorX",
    "MirrorY",
    "FlipY",
    "QuadMirror",
    "FoldX",
    "FoldY",
    "FoldBoth",
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorFoldUvUniforms {
    mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: MirrorFoldUv,
    type_id: "node.mirror",
    purpose: "Mirror/fold coordinate generator: rewrites the per-pixel UV via an axis flip or kaleidoscope-style fold (Identity / Mirror / MirrorX / MirrorY / FlipY / QuadMirror / FoldX / FoldY / FoldBoth) and emits it (R = folded_u, G = folded_v). Resampling at these coordinates produces the mirrored / folded image. Pair with node.remap (Clamp) + node.mix (Lerp) — the TD coordinate → remap → blend shape replacing the fused TransformFX mirror modes. The affine half (translate/scale/rotate) stays in node.transform.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(6),
            range: None,
            enum_values: MIRROR_FOLD_MODES,
        },
    ],
    // depth_rule: zero-input fold-coordinate generator (same shape as centered_uv/kaleidoscope) consumed by a downstream node.remap — no depth origin of its own
    depth_rule: Terminal,
    composition_notes: "Verbatim port of uv_transform.wgsl's mirror pass (modes 1..8, before the affine steps): flips are `1 - uv`; QuadMirror folds both axes onto [0.25, 0.75]; FoldX/FoldY/FoldBoth are the triangle-wave `0.5 - abs(uv - 0.5)` per active axis. Output UVs stay in [0, 1] so remap's Clamp wrap is a no-op safety. Pair: source → mirror_fold_uv → remap(source, uv_field) → mix(source, remapped, Lerp, amount). Default mode 6 (FoldX) matches the legacy Mirror preset default.",
    examples: ["preset.effect.mirror"],
    picker: { label: "Mirror", category: Atom },
    summary: "Folds the image back on itself for mirror reflections, from a simple flip to a four-way quad mirror. It produces the folded coordinates, so feed it into Remap to apply the fold to a picture.",
    category: DistortAndWarp,
    role: Map,
    aliases: ["mirror", "mirror fold", "fold", "quad mirror", "reflect"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/mirror_fold_uv_body.wgsl"),
}

impl Primitive for MirrorFoldUv {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(e)) => *e,
            _ => 6,
        };

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
                    .expect("node.mirror standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mirror",
            )
        });

        let uniforms = MirrorFoldUvUniforms {
            mode,
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
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.mirror",
        );
    }
}
