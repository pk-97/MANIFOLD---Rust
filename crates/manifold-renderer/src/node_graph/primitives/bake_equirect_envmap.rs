//! `node.bake_environment` — procedurally bake an HDR studio
//! environment map at a configurable resolution. Outputs an
//! equirectangular Rgba16Float Texture2D suitable for wiring into
//! `node.render_mesh`'s `envmap` input for PBR-IBL rendering.
//!
//! The studio aesthetic — ambient floor + bright horizon band + overhead
//! softbox + floor fill + two strip lights + azimuthal modulation — is
//! the default look; defaults match the legacy MetallicGlass envmap
//! bit-for-bit at 512×256 (the canonical reference). Width / height /
//! brightness parameters are exposed for future generators that want a
//! different aesthetic without authoring a new primitive.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EnvmapUniforms {
    width: u32,
    height: u32,
    horizon_strength: f32,
    azimuth_variation: f32,
}

crate::primitive! {
    name: BakeEquirectEnvmap,
    type_id: "node.bake_environment",
    purpose: "Procedurally bake an HDR studio environment map at the given resolution. Equirectangular layout (longitude × latitude). Defaults match the legacy MetallicGlass envmap at 512×256: ambient floor + bright horizon band + overhead softbox + floor fill + two strip lights + azimuthal modulation. Output is HDR — wire into `node.render_mesh`'s `envmap` input (PBR material) for IBL reflections, or `node.tone_map` if displaying directly.",
    inputs: {},
    outputs: {
        envmap: Texture2D,
    },
    params: [
        ParamDef {
            name: "width",
            label: "Width",
            ty: ParamType::Int,
            default: ParamValue::Float(512.0),
            range: Some((64.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "height",
            label: "Height",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((32.0, 2048.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "horizon_strength",
            label: "Horizon Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "azimuth_variation",
            label: "Azimuth Variation",
            ty: ParamType::Float,
            default: ParamValue::Float(0.12),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "One-shot per chain rebuild — the runtime allocates a persistent slot for this output; the shader writes once on the first frame and downstream samplers read across frames. Width:Height = 2:1 is the standard equirect ratio (matches asin(y/r) / atan2(z,x) mapping). For non-studio aesthetics author a sibling primitive (sky-gradient, file-loaded HDRI) — this one specifically reproduces the legacy MetallicGlass studio.",
    examples: [],
    picker: { label: "Bake Environment (equirect)", category: Atom },
    summary: "Builds a studio environment map for reflections, laid out as an equirectangular panorama. Feed it into a PBR material for image-based lighting.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["environment map", "bake equirect envmap", "equirect", "ibl", "reflection map"],
}

impl Primitive for BakeEquirectEnvmap {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        params: &ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "envmap" {
            return None;
        }
        let w = match params.get("width") {
            Some(ParamValue::Float(f)) => f.round().max(64.0) as u32,
            _ => 512,
        };
        let h = match params.get("height") {
            Some(ParamValue::Float(f)) => f.round().max(32.0) as u32,
            _ => 256,
        };
        Some((w, h))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_int = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let read_float = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let width = read_int("width", 512.0).round().max(64.0) as u32;
        let height = read_int("height", 256.0).round().max(32.0) as u32;
        let horizon_strength = read_float("horizon_strength", 1.0);
        let azimuth_variation = read_float("azimuth_variation", 0.12);

        let Some(envmap) = ctx.outputs.texture_2d("envmap") else {
            return;
        };
        let tex_width = envmap.width;
        let tex_height = envmap.height;
        if tex_width == 0 || tex_height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/bake_equirect_envmap.wgsl"),
                "cs_main",
                "node.bake_environment",
            )
        });

        let uniforms = EnvmapUniforms {
            width: tex_width.min(width),
            height: tex_height.min(height),
            horizon_strength,
            azimuth_variation,
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
                    texture: envmap,
                },
            ],
            [tex_width.div_ceil(16), tex_height.div_ceil(16), 1],
            "node.bake_environment",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_envmap_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(BakeEquirectEnvmap::TYPE_ID, "node.bake_environment");
        assert!(BakeEquirectEnvmap::INPUTS.is_empty());
        assert_eq!(BakeEquirectEnvmap::OUTPUTS.len(), 1);
        assert_eq!(BakeEquirectEnvmap::OUTPUTS[0].name, "envmap");
        assert_eq!(BakeEquirectEnvmap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    fn params_at(width: f32, height: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert("width", ParamValue::Float(width));
        p.insert("height", ParamValue::Float(height));
        p
    }

    #[test]
    fn output_dims_default_to_512x256() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(512.0, 256.0);
        let dims = node.output_dims("envmap", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((512, 256)));
    }

    #[test]
    fn output_dims_honor_custom_resolution() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(1024.0, 512.0);
        let dims = node.output_dims("envmap", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((1024, 512)));
    }

    #[test]
    fn registers_as_atom() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.bake_environment");
    }
}
