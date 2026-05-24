//! `node.equirect_envmap_sample` — per-pixel image-based-lighting (IBL)
//! reflection. For each pixel: reflect the view direction across the
//! surface normal, then sample an equirectangular environment map at
//! that reflection direction.
//!
//! Pair with `node.bake_equirect_envmap` upstream (or any other
//! equirect-formatted Texture2D — file-loaded HDRIs, procedural sky
//! generators) to supply the env map. Output is the un-attenuated
//! reflected colour; multiply by Fresnel via `node.compose` mode=Multiply
//! and a roughness-scaled coefficient before summing into the final
//! shading.

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
    _pad0: f32,
}

const PBR_BRDF: &str = include_str!("shaders/pbr_brdf.wgsl");

crate::primitive! {
    name: EquirectEnvmapSample,
    type_id: "node.equirect_envmap_sample",
    purpose: "Per-pixel IBL reflection from a tangent-space normal map + equirectangular environment texture. Computes reflect(-view, normal) per pixel and samples the env map at the resulting direction. Returns the raw reflected colour — caller multiplies by Fresnel and roughness scaling before summing into the final shading.",
    inputs: {
        normal: Texture2D required,
        env_map: Texture2D required,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
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
    ],
    composition_notes: "View direction is constant per dispatch — for 3D-mesh perspective use the dedicated PBR mesh-render primitive. The env_map is expected to be in equirectangular (longitude × latitude) layout: pbr_brdf::pbr_equirect_uv maps the 3D reflection direction to texture UV. Output is HDR (can exceed 1.0) — pair with node.tone_map / node.reinhard_tone_map downstream when feeding an SDR display.",
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

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
            return;
        };
        let Some(env_map) = ctx.inputs.texture_2d("env_map") else {
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
    fn has_view_direction_params() {
        let names: Vec<&str> = EquirectEnvmapSample::PARAMS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["view_x", "view_y", "view_z"]);
    }

    #[test]
    fn registers_as_atom() {
        let prim = EquirectEnvmapSample::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.equirect_envmap_sample");
    }
}
