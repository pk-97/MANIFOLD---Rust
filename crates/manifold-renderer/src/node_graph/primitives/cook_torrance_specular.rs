//! `node.cook_torrance_specular` — physically-based microfacet specular
//! from a normal map + light + view + material params. Sibling to
//! `node.blinn_specular`: same shape, more accurate.
//!
//! Computes `D_GGX * G_Smith * F_Schlick / (4 * NdotV * NdotL) * NdotL`
//! per pixel, multiplied by light colour and intensity. Outputs an
//! ADDITIVE specular term — sum with a base shading (`node.lambert_directional`,
//! `node.matcap_two_tone`) via `node.compose` mode=Add.
//!
//! `metallic` ∈ [0, 1] interpolates F0 between dielectric (0.04) and
//! the surface's `base_color`. Metals get a coloured Fresnel; dielectrics
//! get the standard 4% reflectance.
//!
//! Two operating modes, picked implicitly by whether `world_pos` is wired:
//!
//! **Flat-screen mode** (no world_pos): `view_x/y/z` and `light_x/y/z` are
//! interpreted as unit DIRECTIONS, constant across the image. Used by
//! screen-space shaders (OilyFluid-shaped feedback sims) — one V and one L
//! shared by every pixel.
//!
//! **3D-mesh mode** (world_pos wired): `view_x/y/z` is interpreted as the
//! CAMERA WORLD POSITION; `light_x/y/z` as the LIGHT WORLD POSITION.
//! Per-pixel V and L are computed from `camera_pos - world_pos` and
//! `light_pos - world_pos`. An inverse-square attenuation
//! `1 / (1 + d²/attenuation_scale)` is multiplied into the result so a
//! positional light falls off realistically. Wire this with
//! `node.render_3d_mesh.world_pos` + `node.render_3d_mesh.world_normal`
//! to shade a perspective-projected mesh with per-pixel PBR. `normal` in
//! 3D mode is the world-space surface normal (from `world_normal`), not a
//! tangent-space normal map.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CookTorranceUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    roughness: f32,
    view_x: f32,
    view_y: f32,
    view_z: f32,
    metallic: f32,
    light_color: [f32; 4], // rgb = colour, a = intensity
    base_color: [f32; 4],
    use_world_pos: u32,    // 0 = flat screen-space, 1 = 3D mesh (per-pixel V/L)
    use_roughness_map: u32,
    attenuation_scale: f32,
    _pad0: f32,
}

const PBR_BRDF: &str = include_str!("shaders/pbr_brdf.wgsl");

crate::primitive! {
    name: CookTorranceSpecular,
    type_id: "node.cook_torrance_specular",
    purpose: "Physically-based microfacet specular (D_GGX × G_Smith × F_Schlick) from a tangent-space normal map + directional light + view + material. Outputs the ADDITIVE specular contribution. Sibling to node.blinn_specular — more accurate for metals; pair with node.lambert_directional for the diffuse term and sum via node.compose mode=Add. F0 interpolates between dielectric (0.04) and base_color by metallic. Wire a `node.light` into `light` to drive direction (Sun) or position (Point, when world_pos is also wired) + colour from one source instead of scattered scalars.",
    inputs: {
        normal: Texture2D required,
        world_pos: Texture2D optional,
        roughness_map: Texture2D optional,
        light: Light optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
        roughness: ScalarF32 optional,
        metallic: ScalarF32 optional,
        intensity: ScalarF32 optional,
        attenuation_scale: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "light_x",
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.35),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_y",
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_z",
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "view_x",
            label: "View X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "view_y",
            label: "View Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "view_z",
            label: "View Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "roughness",
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.01, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "metallic",
            label: "Metallic",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "intensity",
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_color",
            label: "Light Color",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "base_color",
            label: "Base Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.8, 0.8, 0.82, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "attenuation_scale",
            label: "Attenuation Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(25.0),
            range: Some((0.1, 1000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output is the additive specular term — sum with diffuse via `node.compose` mode=Add. base_color is the metal tint when metallic=1; for dielectrics (metallic=0) it's ignored. roughness clamps to 0.01 to avoid mirror-degenerate. \n\nFlat-screen mode (no world_pos input): normal is a tangent-space normal map, view/light are constant directions. \n\n3D-mesh mode (world_pos wired from node.render_3d_mesh.world_pos): normal is a world-space surface normal (from node.render_3d_mesh.world_normal), view scalars are camera world position, light scalars are light world position, attenuation_scale (default 25.0) shapes the 1/(1+d²/scale) falloff. Background pixels (world_pos.a < 0.5) emit zero.",
    examples: [],
    picker: { label: "Cook-Torrance Specular", category: Atom },
}

impl Primitive for CookTorranceSpecular {
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
        let roughness = read("roughness", 0.3);
        let metallic = read("metallic", 1.0);
        let mut intensity = read("intensity", 1.0);
        let mut attenuation_scale = read("attenuation_scale", 25.0);
        let mut light_color = match ctx.params.get("light_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [1.0, 1.0, 1.0, 1.0],
        };
        let base_color = match ctx.params.get("base_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.8, 0.8, 0.82, 1.0],
        };

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
            return;
        };
        let world_pos = ctx.inputs.texture_2d("world_pos");
        let roughness_map = ctx.inputs.texture_2d("roughness_map");

        // Wired `node.light` overrides direction/position + colour. In
        // flat-screen mode (world_pos not wired) we feed `light_x/y/z`
        // as DIRECTION (negate `light.dir` to match the existing
        // convention: scalars point FROM scene TOWARD light). In
        // 3D-mesh mode (world_pos wired) we feed them as world POSITION
        // (the WGSL switches on `use_world_pos` and recomputes L per
        // pixel as `normalize(light_pos - world_pos)`). `light.color` is
        // pre-multiplied with intensity by the producer, so we set
        // intensity=1 to avoid double-multiplying. `light.range` maps
        // to attenuation_scale (Point light → the 1/(1+d²/scale)
        // falloff; Sun light is parallel rays so attenuation_scale
        // doesn't affect output in flat-screen mode but is harmlessly
        // passed through).
        if let Some(light) = ctx.inputs.light("light") {
            if world_pos.is_some() {
                light_x = light.pos[0];
                light_y = light.pos[1];
                light_z = light.pos[2];
            } else {
                light_x = -light.dir[0];
                light_y = -light.dir[1];
                light_z = -light.dir[2];
            }
            light_color = [light.color[0], light.color[1], light.color[2], 1.0];
            intensity = 1.0;
            attenuation_scale = light.range;
        }
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let source = format!(
                "{}\n{}",
                PBR_BRDF,
                include_str!("shaders/cook_torrance_specular.wgsl"),
            );
            gpu.device.create_compute_pipeline(
                &source,
                "cs_main",
                "node.cook_torrance_specular",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = CookTorranceUniforms {
            light_x,
            light_y,
            light_z,
            roughness,
            view_x,
            view_y,
            view_z,
            metallic,
            light_color: [
                light_color[0],
                light_color[1],
                light_color[2],
                intensity,
            ],
            base_color,
            use_world_pos: world_pos.is_some() as u32,
            use_roughness_map: roughness_map.is_some() as u32,
            attenuation_scale,
            _pad0: 0.0,
        };

        // Shader always binds world_pos + roughness_map slots; bind `target`
        // as a harmless dummy when an input isn't wired (the shader gates
        // the read behind the matching `use_*` flag).
        let world_pos_bind = world_pos.unwrap_or(target);
        let roughness_map_bind = roughness_map.unwrap_or(target);

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
                GpuBinding::Texture {
                    binding: 4,
                    texture: world_pos_bind,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: roughness_map_bind,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1,],
            "node.cook_torrance_specular",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn cook_torrance_declares_normal_input_and_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            CookTorranceSpecular::TYPE_ID,
            "node.cook_torrance_specular"
        );
        assert_eq!(CookTorranceSpecular::INPUTS[0].name, "normal");
        assert_eq!(CookTorranceSpecular::INPUTS[0].ty, PortType::Texture2D);
        assert!(CookTorranceSpecular::INPUTS[0].required);
        assert_eq!(CookTorranceSpecular::INPUTS[1].name, "world_pos");
        assert_eq!(CookTorranceSpecular::INPUTS[1].ty, PortType::Texture2D);
        assert!(!CookTorranceSpecular::INPUTS[1].required);
        assert_eq!(CookTorranceSpecular::OUTPUTS.len(), 1);
        assert_eq!(CookTorranceSpecular::OUTPUTS[0].name, "out");
    }

    #[test]
    fn cook_torrance_has_pbr_params() {
        let names: Vec<&str> = CookTorranceSpecular::PARAMS
            .iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"roughness"));
        assert!(names.contains(&"metallic"));
        assert!(names.contains(&"base_color"));
        assert!(names.contains(&"light_color"));
    }

    #[test]
    fn registers_as_atom_palette_entry() {
        let prim = CookTorranceSpecular::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.cook_torrance_specular");
    }
}
