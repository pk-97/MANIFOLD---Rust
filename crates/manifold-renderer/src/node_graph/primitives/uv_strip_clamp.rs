//! `node.edge_stretch` — edge-stretch coordinate generator. Clamps the
//! per-pixel UV to a center strip of width `width` on the selected axis
//! (Horiz / Vert / Both). Resampling at the clamped coordinates stretches
//! the edge row/column outward. Pair with `node.remap` + `node.mix` — the
//! TD `coordinate → remap → blend` shape replacing the fused
//! `node.edge_stretch` kernel.
//!
//! Output: R = clamped_u, G = clamped_v, B = 0, A = 1. Verbatim port of
//! the legacy edge-stretch clamp, so `remap(Clamp) + mix(Lerp)` reproduces
//! it bit-for-bit.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const UV_STRIP_CLAMP_MODES: &[&str] = &["Horiz", "Vert", "Both"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UvStripClampUniforms {
    width: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: UvStripClamp,
    type_id: "node.edge_stretch",
    purpose: "Edge-stretch coordinate generator: clamps the per-pixel UV to a center strip of width `width` on the selected axis (Horiz / Vert / Both) and emits it (R = clamped_u, G = clamped_v). Resampling at these coordinates stretches the edge row/column outward. Pair with node.remap (Clamp) + node.mix (Lerp) — the TD coordinate → remap → blend shape replacing the fused node.edge_stretch kernel.",
    inputs: {
        width: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.1, 0.9)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("direction"),
            label: "Direction",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: UV_STRIP_CLAMP_MODES,
        },
    ],
    // depth_rule: zero-input UV-coordinate clamp generator, same group as centered_uv/uv_field
    depth_rule: Terminal,
    composition_notes: "Verbatim port of the legacy node.edge_stretch clamp: half_width = width * 0.5, strip = [0.5 - hw, 0.5 + hw], clamped per active axis. `width` is port-shadowed (legacy `source_width` binding). Output UVs stay in range so remap's Clamp wrap is a no-op safety. Pair: source → uv_strip_clamp → remap(source, uv_field) → mix(source, remapped, Lerp, amount).",
    examples: ["preset.effect.edge_stretch"],
    picker: { label: "Edge Stretch", category: Atom },
    summary: "Grabs a thin strip across the middle of the frame and smears it out to the edges, the classic slit-scan stretch. It outputs coordinates, so pair it with Remap.",
    category: DistortAndWarp,
    role: Map,
    aliases: ["edge stretch", "strip clamp", "slit scan", "smear", "stretch"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/uv_strip_clamp_body.wgsl"),
}

impl Primitive for UvStripClamp {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let width = ctx.scalar_or_param("width", 0.5);
        let mode = match ctx.params.get("direction") {
            Some(ParamValue::Enum(e)) => *e,
            _ => 0,
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
            // Source generator: 0 texture inputs, output at binding 1. Generated
            // kernel binds uniform(0)/dst(1). uv_strip_clamp.wgsl is the oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.edge_stretch standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.edge_stretch",
            )
        });

        let uniforms = UvStripClampUniforms {
            width,
            mode,
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
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.edge_stretch",
        );
    }
}
