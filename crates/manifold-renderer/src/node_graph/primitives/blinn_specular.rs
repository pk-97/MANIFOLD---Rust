//! `node.shininess` — Blinn-Phong specular from a tangent-space
//! normal + light + view. Pair with `node.matcap_two_tone` (base) +
//! `node.rim_light` (rim) for full stylised PBR layering.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// WGSL alignment: each vec3 occupies 16 bytes (12 data + 4 pad). Total 48.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
// Field order MUST match the PARAMS declaration order below — the runtime
// pipeline is the codegen-generated kernel (`standalone_for_spec`), whose
// uniform layout is derived from PARAMS in order. BUG (found P7, 2026-07-18):
// this struct had `power` 4th while PARAMS declare view_x/y/z before `power`,
// so the standalone dispatch fed the kernel view=(48,0,0), power=1.0 — the
// production "weird tints" specular. The gpu_tests parity oracle packs its
// own bytes per kernel, so it never exercised THIS struct.
struct BlinnUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    // A Color param expands to four CONSECUTIVE f32 fields in the generated
    // layout (no vec4 alignment), so color sits directly after `power`; the
    // trailing pad rounds the buffer to naga's 16-byte uniform size multiple.
    color: [f32; 4],
    _pad0: f32,
}

crate::primitive! {
    name: BlinnSpecular,
    type_id: "node.shininess",
    purpose: "Blinn-Phong specular from a tangent-space normal map + directional light + view: `h = normalize(light + view); spec = pow(max(dot(n, h), 0), power)`. ADDITIVE — sum with a base shading via `node.compose` mode=Add. Defaults match oily-fluid PBR (light=(0.35,0.55,0.75), view=(0,0,1), power=48, near-white tint). Wire a `node.light` into `light` to drive direction + colour from one source instead of scattered scalars; the wired light's colour multiplies the `color` tint param.",
    inputs: {
        normal: Texture2D required,
        light: Light optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
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
            name: Cow::Borrowed("light_x"),
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.35),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_y"),
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_z"),
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
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
            default: ParamValue::Float(48.0),
            range: Some((1.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 0.95, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    // depth_rule: reads a normal map (not color) and a Light; pointwise same-texel shading calc with no UV remap, classified structurally like the other lighting atoms (basic_light, rim_light, matcap_two_tone)
    depth_rule: Inherit,
    composition_notes: "Both light and view are normalised in-shader. Color alpha is ignored; output alpha = spec weight. For audio-reactive sparkle, wire a beat-driven LFO into `power` (lower power = broader highlight; higher = pinpoint). When `light` is wired, the light's direction overrides `light_x/y/z`; the light's pre-multiplied colour multiplies into the `color` tint (so a yellow light through a magenta surface gives a yellow×magenta highlight).",
    examples: [],
    picker: { label: "Shininess (Blinn)", category: Atom },
    summary: "Adds a tight highlight where the surface catches the light, set by a shininess amount. The glossy hotspot on top of basic lighting.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["specular", "shininess", "blinn specular", "highlight", "blinn"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/blinn_specular_body.wgsl"),
}

impl Primitive for BlinnSpecular {
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
        let mut light_x = read("light_x", 0.35);
        let mut light_y = read("light_y", 0.55);
        let mut light_z = read("light_z", 0.75);
        let view_x = read("view_x", 0.0);
        let view_y = read("view_y", 0.0);
        let view_z = read("view_z", 1.0);
        let power = read("power", 48.0);
        let mut color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [1.0, 0.95, 1.0, 1.0],
        };

        // Wired `node.light` overrides direction (negate `light.dir`
        // to match the existing scalar convention) and multiplies into
        // the colour tint. No world_pos mode on blinn — point lights
        // collapse to their forward direction at every pixel.
        if let Some(light) = ctx.inputs.light("light") {
            light_x = -light.dir[0];
            light_y = -light.dir[1];
            light_z = -light.dir[2];
            color = [
                color[0] * light.color[0],
                color[1] * light.color[1],
                color[2] * light.color[2],
                color[3],
            ];
        }

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
            // blinn_specular.wgsl` (the hand-kernel parity oracle) was
            // deleted 2026-07-20 (W1-B, migration scaffolding retired).
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.shininess standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.shininess",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = BlinnUniforms {
            light_x,
            light_y,
            light_z,
            power,
            view_x,
            view_y,
            view_z,
            _pad0: 0.0,
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
            "node.shininess",
        );
    }
}
