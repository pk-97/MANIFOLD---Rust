//! `node.rim_light` — fresnel-based edge highlight from a
//! tangent-space normal map. Black at face-on, `color`-tinted at
//! grazing. ADDITIVE — sum with a base shading via `node.compose`
//! mode=Add (or mode=Screen for HDR-safe).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FresnelUniforms {
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    color: [f32; 4],
}

crate::primitive! {
    name: FresnelRim,
    type_id: "node.rim_light",
    purpose: "Fresnel-based edge highlight from a tangent-space normal map: `f = pow(1 - max(dot(n, view), 0), power)`, output = color.rgb * f. ADDITIVE rim term — black at face-on, `color`-tinted at grazing. Sum with a base shading (matcap, lambert) via `node.compose` mode=Add to layer the rim onto the surface. Defaults match oily-fluid PBR (view=(0,0,1), power=3, color=iridescent magenta).",
    inputs: {
        normal: Texture2D required,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
        power: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("view_x"),
            label: "View X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("view_y"),
            label: "View Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("view_z"),
            label: "View Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("power"),
            label: "Power",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.55, 0.30, 0.85, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "view defaults to (0, 0, 1) — camera looking down +Z. Higher `power` sharpens the rim. Color alpha is ignored; output alpha = fresnel weight (handy for compositing the rim only where it's actually contributing). Port-shadowed `power` lets you pulse the rim from a beat or LFO.",
    examples: [],
    picker: { label: "Rim Light (Fresnel)", category: Atom },
    summary: "Lights up the edges of a surface where it turns away from the camera, the glowing rim you see on backlit objects.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["rim light", "fresnel rim", "fresnel", "edge glow", "backlight"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/fresnel_rim_body.wgsl"),
}

impl Primitive for FresnelRim {
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
        let view_x = read("view_x", 0.0);
        let view_y = read("view_y", 0.0);
        let view_z = read("view_z", 1.0);
        let power = read("power", 3.0);
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.55, 0.30, 0.85, 1.0],
        };

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
            // generated from `wgsl_body` so the atom fuses.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.rim_light standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rim_light",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FresnelUniforms {
            view_x,
            view_y,
            view_z,
            power,
            color,
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
            "node.rim_light",
        );
    }
}
