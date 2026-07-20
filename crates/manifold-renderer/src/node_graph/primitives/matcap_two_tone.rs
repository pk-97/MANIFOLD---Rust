//! `node.matcap_two_tone` — cross-axis 4-colour matcap from a
//! tangent-space normal map.
//!
//! Two 2-tone gradients (one along normal.x, one along normal.y) summed
//! by axis. The stylised PBR base atom — pair upstream with
//! `node.surface_bumps` and downstream with `node.rim_light`
//! + `node.shininess` summed for the full PBR look.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatcapUniforms {
    color_y_low: [f32; 4],
    color_y_high: [f32; 4],
    color_x_low: [f32; 4],
    color_x_high: [f32; 4],
}

crate::primitive! {
    name: MatcapTwoTone,
    type_id: "node.matcap_two_tone",
    purpose: "Cross-axis 4-colour matcap from a tangent-space normal map. Per pixel: mc=n.xy*0.5+0.5, base=mix(y_low, y_high, mc.y), side=mix(x_low, x_high, mc.x), out=(base+side)*0.5. Two 2-tone gradients per axis combined for a 4-corner matcap look. Defaults reproduce oily-fluid's PBR base palette (deep purple → pale blue Y axis, magenta → teal X axis).",
    inputs: {
        normal: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color_y_low"),
            label: "Y Low (shadow)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.08, 0.05, 0.22, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_y_high"),
            label: "Y High (highlight)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.55, 0.75, 0.95, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_x_low"),
            label: "X Low (left)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.25, 0.10, 0.45, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_x_high"),
            label: "X High (right)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.15, 0.55, 0.60, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Output is fully opaque RGB. Sum with `node.rim_light` (additive) and `node.shininess` (additive) via `node.compose` (mode=Add) to build the full stylised-PBR shading layer. For a single-axis 2-tone matcap, set the unused-axis colors equal (e.g. color_x_low = color_x_high) — the side contribution becomes a constant added to the base.",
    examples: [],
    picker: { label: "Matcap Two-Tone", category: Atom },
    summary: "Shades a surface by mapping its normals into a two-tone sphere lookup, a fast stylised material that needs no real lights.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["matcap", "two tone", "sphere map", "lit sphere"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matcap_two_tone_body.wgsl"),
}

impl Primitive for MatcapTwoTone {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_color = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Color(c)) => *c,
                _ => default,
            }
        };
        let color_y_low = read_color("color_y_low", [0.08, 0.05, 0.22, 1.0]);
        let color_y_high = read_color("color_y_high", [0.55, 0.75, 0.95, 1.0]);
        let color_x_low = read_color("color_x_low", [0.25, 0.10, 0.45, 1.0]);
        let color_x_high = read_color("color_x_high", [0.15, 0.55, 0.60, 1.0]);

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
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
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/
            // matcap_two_tone.wgsl` (the hand-kernel parity oracle) was
            // deleted 2026-07-20 (W1-B, migration scaffolding retired).
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.matcap_two_tone standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.matcap_two_tone",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MatcapUniforms {
            color_y_low,
            color_y_high,
            color_x_low,
            color_x_high,
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
                    texture: normal,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.matcap_two_tone",
        );
    }
}
