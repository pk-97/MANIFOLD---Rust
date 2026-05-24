//! `node.cook_torrance_specular` — physically-based microfacet specular
//! from a tangent-space normal map + directional light + view + material
//! params. Sibling to `node.blinn_specular`: same shape, more accurate.
//!
//! Computes `D_GGX * G_Smith * F_Schlick / (4 * NdotV * NdotL) * NdotL`
//! per pixel, multiplied by light colour and intensity. Outputs an
//! ADDITIVE specular term — sum with a base shading (`node.lambert_directional`,
//! `node.matcap_two_tone`) via `node.compose` mode=Add.
//!
//! `metallic` ∈ [0, 1] interpolates F0 between dielectric (0.04) and
//! the surface's `base_color`. Metals get a coloured Fresnel; dielectrics
//! get the standard 4% reflectance.

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
}

const PBR_BRDF: &str = include_str!("shaders/pbr_brdf.wgsl");

crate::primitive! {
    name: CookTorranceSpecular,
    type_id: "node.cook_torrance_specular",
    purpose: "Physically-based microfacet specular (D_GGX × G_Smith × F_Schlick) from a tangent-space normal map + directional light + view + material. Outputs the ADDITIVE specular contribution. Sibling to node.blinn_specular — more accurate for metals; pair with node.lambert_directional for the diffuse term and sum via node.compose mode=Add. F0 interpolates between dielectric (0.04) and base_color by metallic.",
    inputs: {
        normal: Texture2D required,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
        roughness: ScalarF32 optional,
        metallic: ScalarF32 optional,
        intensity: ScalarF32 optional,
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
    ],
    composition_notes: "Output is the additive specular term — sum with diffuse via `node.compose` mode=Add. base_color is the metal tint when metallic=1; for dielectrics (metallic=0) it's ignored. roughness clamps to 0.01 to avoid mirror-degenerate. Normal map is tangent-space, signed (typically from node.heightmap_to_normal). View direction is constant per dispatch — for 3D-mesh perspective use the dedicated mesh-render primitive that varies view per pixel.",
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
        let light_x = read("light_x", 0.35);
        let light_y = read("light_y", 0.55);
        let light_z = read("light_z", 0.75);
        let view_x = read("view_x", 0.0);
        let view_y = read("view_y", 0.0);
        let view_z = read("view_z", 1.0);
        let roughness = read("roughness", 0.3);
        let metallic = read("metallic", 1.0);
        let intensity = read("intensity", 1.0);
        let light_color = match ctx.params.get("light_color") {
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
