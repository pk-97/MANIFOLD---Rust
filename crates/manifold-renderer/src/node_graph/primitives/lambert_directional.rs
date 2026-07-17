//! `node.basic_light` — Lambert (diffuse) lighting from a
//! tangent-space normal map + directional light + ambient floor.
//!
//! Output is RGB [0, 1] — grayscale when no `light` wire is bound (or
//! when the wired light's colour is white), tinted by the light's
//! colour when one is wired. Caller tints further downstream with
//! `node.color_grade` / `node.gradient_map` if needed.
//!
//! Two ways to drive the light:
//!
//! 1. **Scalar params (default):** the scattered `light_x/y/z` +
//!    `ambient` scalars drive a directional light at `(light_x, light_y,
//!    light_z)`. Output is grayscale (`light_color` defaults to white).
//!    Backwards-compatible with every existing OilyFluid-shaped consumer.
//! 2. **`light: Light` wire:** wire a `node.light` into the `light`
//!    input. The light's pre-normalised direction and premultiplied
//!    colour are used instead of the scattered scalars; `ambient` still
//!    comes from the scalar param. For point lights in this flat-screen
//!    atom, the light's forward `dir` is used at every pixel (no
//!    per-pixel attenuation — that needs a `world_pos`-wired 3D-mesh-mode
//!    extension, which this atom doesn't have).

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// Layout matches the WGSL struct: two vec3<f32> components (each padded
// to 16 bytes via a trailing f32). Total 32 bytes — same size as the
// pre-Light-input version; the old trailing `_pad0..2` slots now carry
// the light colour.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LambertUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    _vec3_pad: f32,
    light_color_r: f32,
    light_color_g: f32,
    light_color_b: f32,
    ambient: f32,
}

crate::primitive! {
    name: LambertDirectional,
    type_id: "node.basic_light",
    purpose: "Lambert (diffuse) shading from a tangent-space normal map and a directional light: `out = max(dot(n, normalize(light_dir)), 0) * (1-ambient) + ambient`, multiplied by the light's colour. The basic directional-lighting atom — pair with `node.gradient_map` to tint further, or sum with `node.rim_light` / `node.shininess` for stylized PBR. Two ways to drive the light: scattered `light_x/y/z` scalars (output grayscale, white tint), or wire a `node.light` into `light` (output picks up the light's premultiplied colour). The scalar fallback keeps every existing OilyFluid-shaped consumer working unchanged.",
    inputs: {
        normal: Texture2D required,
        light: Light optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        ambient: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("light_x"),
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_y"),
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_z"),
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("ambient"),
            label: "Ambient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Light direction is normalised in-shader so any non-zero (x, y, z) works. Default light is over-the-shoulder camera (0.4, 0.6, 0.7). Port-shadowed components let you wire an LFO or `node.color_compass` to orbit the light at performance time. If a `node.light` is wired into `light`, its direction + colour override the scattered `light_x/y/z` scalars; `ambient` still comes from the scalar param so an unwired Light input doesn't strand it. Point lights wire-in as a single direction (no per-pixel attenuation in this flat-screen atom) — for per-pixel attenuation and full PBR, use `node.render_mesh` with a `node.pbr_material` (the material node owns the lighting model).",
    examples: [],
    picker: { label: "Basic Light (Lambert)", category: Atom },
    summary: "Shades a surface from its normal map and a single direction, brightest where it faces the light. The plain matte lighting term.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["lambert", "lambert directional", "diffuse", "matte", "basic light"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/lambert_directional_body.wgsl"),
}

impl Primitive for LambertDirectional {
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

        // Scalar-only fallback values (used unless a Light wire overrides them).
        let mut light_x = read("light_x", 0.4);
        let mut light_y = read("light_y", 0.6);
        let mut light_z = read("light_z", 0.7);
        let mut light_color = [1.0_f32, 1.0, 1.0];
        let ambient = read("ambient", 0.1);

        // Wired `node.light` overrides direction + colour. Both Sun and
        // Point lights collapse to `light.dir` in this flat-screen atom
        // (no per-pixel world_pos → no per-pixel L for point lights);
        // the wired direction points FROM the light TOWARD the scene,
        // so we negate it to match the existing scalar convention where
        // `light_x/y/z` points FROM the scene TOWARD the light.
        if let Some(light) = ctx.inputs.light("light") {
            light_x = -light.dir[0];
            light_y = -light.dir[1];
            light_z = -light.dir[2];
            light_color = [light.color[0], light.color[1], light.color[2]];
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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/lambert_directional.wgsl"),
                "cs_main",
                "node.basic_light",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = LambertUniforms {
            light_x,
            light_y,
            light_z,
            _vec3_pad: 0.0,
            light_color_r: light_color[0],
            light_color_g: light_color[1],
            light_color_b: light_color[2],
            ambient,
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
            "node.basic_light",
        );
    }
}
