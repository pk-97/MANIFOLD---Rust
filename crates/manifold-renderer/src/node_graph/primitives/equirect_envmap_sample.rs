//! `node.equirect_envmap_sample` — per-pixel image-based-lighting (IBL)
//! reflection. For each pixel: reflect the view direction across the
//! surface normal, sample an equirectangular environment map at that
//! reflection direction, then apply Schlick Fresnel + roughness scaling
//! so the output drops in for the IBL term of a Cook-Torrance PBR sum.
//!
//! Pair with `node.bake_equirect_envmap` upstream (or any other
//! equirect-formatted Texture2D — file-loaded HDRIs, procedural sky
//! generators) to supply the env map.
//!
//! Two operating modes, picked implicitly by whether `world_pos` is wired:
//!
//! **Flat-screen mode** (no world_pos): `view_x/y/z` is a constant unit
//! direction shared across every pixel. Used by screen-space PBR shaders
//! over a flat surface.
//!
//! **3D-mesh mode** (world_pos wired): `view_x/y/z` is the camera world
//! position; per-pixel V comes from `camera_pos - world_pos`. `normal`
//! in this mode is the world-space surface normal (typically from
//! `node.render_3d_mesh.world_normal`). Background pixels
//! (world_pos.a < 0.5) emit zero.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EnvSampleUniforms {
    view_x: f32,
    view_y: f32,
    view_z: f32,
    roughness: f32,
    base_color: [f32; 4],
    metallic: f32,
    use_world_pos: u32,
    roughness_scale: f32,
    use_roughness_map: u32,
}

const PBR_BRDF: &str = include_str!("shaders/pbr_brdf.wgsl");

crate::primitive! {
    name: EquirectEnvmapSample,
    type_id: "node.equirect_envmap_sample",
    purpose: "Per-pixel IBL reflection from a tangent-space normal map + equirectangular environment texture. Computes reflect(-view, normal) per pixel and samples the env map at the resulting direction. Returns the raw reflected colour — caller multiplies by Fresnel and roughness scaling before summing into the final shading.",
    inputs: {
        normal: Texture2D required,
        env_map: Texture2D required,
        world_pos: Texture2D optional,
        roughness_map: Texture2D optional,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
        roughness: ScalarF32 optional,
        metallic: ScalarF32 optional,
        roughness_scale: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "view_x",
            label: "View X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-20.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "view_y",
            label: "View Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-20.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "view_z",
            label: "View Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-20.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "roughness",
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
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
            name: "roughness_scale",
            label: "Roughness Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((0.0, 1.0)),
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
    composition_notes: "Output is `env * F_schlick(NdotV, F0) * (1 - roughness * roughness_scale)` per pixel — the IBL term of a Cook-Torrance PBR sum. F0 = mix(0.04, base_color, metallic). Sum with `node.cook_torrance_specular`'s direct-lighting output via `node.compose` mode=Add for the full PBR shading. env_map is equirectangular (longitude × latitude); pbr_equirect_uv maps reflection direction to UV. Output is HDR — pair with `node.reinhard_tone_map` for SDR display. The legacy `roughness_scale = 0.7` matches the MetallicGlass bundle exactly.",
    examples: [],
    picker: { label: "Env Reflect (Equirect)", category: Atom },
}

impl Primitive for EquirectEnvmapSample {
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
        let roughness = read("roughness", 0.05);
        let metallic = read("metallic", 1.0);
        let roughness_scale = read("roughness_scale", 0.7);
        let base_color = match ctx.params.get("base_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.8, 0.8, 0.82, 1.0],
        };

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
            return;
        };
        let Some(env_map) = ctx.inputs.texture_2d("env_map") else {
            return;
        };
        let world_pos = ctx.inputs.texture_2d("world_pos");
        let roughness_map = ctx.inputs.texture_2d("roughness_map");
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
                include_str!("shaders/equirect_envmap_sample.wgsl"),
            );
            gpu.device.create_compute_pipeline(
                &source,
                "cs_main",
                "node.equirect_envmap_sample",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = EnvSampleUniforms {
            view_x,
            view_y,
            view_z,
            roughness,
            base_color,
            metallic,
            use_world_pos: world_pos.is_some() as u32,
            roughness_scale,
            use_roughness_map: roughness_map.is_some() as u32,
        };

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
                GpuBinding::Texture {
                    binding: 2,
                    texture: env_map,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: world_pos_bind,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: roughness_map_bind,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.equirect_envmap_sample",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_normal_and_env_inputs() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            EquirectEnvmapSample::TYPE_ID,
            "node.equirect_envmap_sample"
        );
        assert!(EquirectEnvmapSample::INPUTS.len() >= 2);
        assert_eq!(EquirectEnvmapSample::INPUTS[0].name, "normal");
        assert_eq!(EquirectEnvmapSample::INPUTS[0].ty, PortType::Texture2D);
        assert!(EquirectEnvmapSample::INPUTS[0].required);
        assert_eq!(EquirectEnvmapSample::INPUTS[1].name, "env_map");
        assert_eq!(EquirectEnvmapSample::INPUTS[1].ty, PortType::Texture2D);
        assert!(EquirectEnvmapSample::INPUTS[1].required);
    }

    #[test]
    fn has_view_pbr_and_base_color_params() {
        let names: Vec<&str> = EquirectEnvmapSample::PARAMS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "view_x",
                "view_y",
                "view_z",
                "roughness",
                "metallic",
                "roughness_scale",
                "base_color",
            ]
        );
    }

    #[test]
    fn registers_as_atom() {
        let prim = EquirectEnvmapSample::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.equirect_envmap_sample");
    }
}
