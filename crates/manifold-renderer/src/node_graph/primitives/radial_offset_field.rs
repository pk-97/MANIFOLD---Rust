//! `node.radial_offset_field` — directional displacement field generator.
//!
//! Emits a per-pixel 2D direction (R = x, G = y, signed) that points either
//! radially outward from the center (with a center→edge falloff mask) or
//! along a fixed angle. The reusable field behind the whole radial-warp
//! family — chromatic aberration, lens distortion, zoom-blur direction,
//! radial smear — anything that needs "which way does this pixel push, and
//! how hard."
//!
//! The radial branch is a verbatim port of the legacy chromatic-aberration
//! direction math, so chromatic aberration decomposes to
//! `radial_offset_field → node.rgb_split → node.mix` without
//! changing the look. The offset magnitude and ± sign live on the consumer
//! (scale its `amount`/`weight`); this node emits the unit-ish direction
//! (|dir| ≤ 1) only.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RadialOffsetFieldUniforms {
    mode: u32,
    angle: f32,
    falloff: f32,
    _pad0: f32,
}

crate::primitive! {
    name: RadialOffsetField,
    type_id: "node.radial_offset_field",
    purpose: "Directional displacement field generator. Radial mode: per-pixel direction points outward from the center, scaled by a center→edge falloff mask. Linear mode: a uniform direction at `angle` degrees. Output R = dir.x, G = dir.y (signed), B = 0, A = 1; |dir| ≤ 1. The reusable direction field behind the radial-warp family — feed it as the velocity field to node.rgb_split (chromatic aberration), node.uv_displace_by_flow (lens / zoom warp), node.texture_advect. The displacement magnitude and ± sign are applied by the consumer.",
    inputs: {
        angle: ScalarF32 optional,
        falloff: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: &["Radial", "Linear"],
        },
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("falloff"),
            label: "Falloff",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Radial mask is a verbatim port of the legacy chromatic-aberration math: smoothstep(0, 0.707, dist) then mix(mask, 1, 1 - falloff) — falloff=1 keeps the full center→edge ramp, falloff=0 makes the field uniform. Near-center (dist < 1e-5) falls back to (1, 0). `angle` (Linear) and `falloff` (Radial) are port-shadowed for per-frame drive. The output is the OUTWARD/forward direction; for chromatic aberration the consumer (chromatic_displace) is fed a negated, pixel-scaled amount so red samples outward like the legacy effect. Pair: radial_offset_field → chromatic_displace(velocity) with amount = -(offset · ~1440px) → mix(source, split, Lerp, amount).",
    examples: ["preset.effect.chromatic_aberration"],
    picker: { label: "Radial Offset Field", category: Atom },
    summary: "Makes a push outward from a centre point that other nodes use to shift pixels. It has no look of its own, so wire it into a displace or remap node.",
    category: DistortAndWarp,
    role: Map,
    aliases: ["push field", "radial displace", "zoom warp"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/radial_offset_field_body.wgsl"),
}

impl Primitive for RadialOffsetField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let angle = ctx.scalar_or_param("angle", 0.0);
        let falloff = ctx.scalar_or_param("falloff", 0.5);

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
            // kernel binds uniform(0)/dst(1). radial_offset_field.wgsl is the oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.radial_offset_field standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.radial_offset_field",
            )
        });

        let uniforms = RadialOffsetFieldUniforms {
            mode,
            angle,
            falloff,
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
            "node.radial_offset_field",
        );
    }
}
