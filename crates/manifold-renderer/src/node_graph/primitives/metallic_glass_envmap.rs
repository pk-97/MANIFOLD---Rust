//! `node.metallic_glass_envmap` — bake the procedural HDR studio
//! environment map used by MetallicGlass.
//!
//! Bit-exact wrap of `generators/shaders/metallic_glass_envmap.wgsl`
//! via include_str. Zero inputs, one Texture2D output (512×256
//! equirectangular RGBA16F). The shader bakes a specific "studio"
//! aesthetic (ambient floor + horizon band + overhead softbox +
//! floor fill + two strip lights + azimuthal variation) — useful
//! mainly for MetallicGlass parity in JSON decomposition.
//!
//! For a parametrised general-purpose HDR envmap, a separate
//! primitive should be authored (this one matches the legacy
//! aesthetic bit-for-bit).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: MetallicGlassEnvmap,
    type_id: "node.metallic_glass_envmap",
    purpose: "Bake the procedural HDR studio environment map used by MetallicGlass's PBR rendering. Output is a 512×256 equirectangular Rgba16Float texture. Bit-exact extraction of metallic_glass_envmap.wgsl; primarily used for MetallicGlass JSON decomposition. Pair with node.pbr_render or node.metallic_glass_render downstream.",
    inputs: {},
    outputs: {
        envmap: Texture2D,
    },
    params: [],
    composition_notes: "Output dimensions are 512×256 (hardcoded in the shader to match the legacy MetallicGlass envmap). The aesthetic is specifically a studio with horizon band + overhead softbox + floor fill + two strip lights — useful for chrome / metallic surfaces. Dispatch this once per chain rebuild; the result is static (no per-frame uniforms). For other envmap aesthetics, author a new primitive.",
    examples: [],
    picker: { label: "Metallic Glass Envmap", category: Atom },
}

impl Primitive for MetallicGlassEnvmap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(envmap) = ctx.outputs.texture_2d("envmap") else {
            return;
        };
        let width = envmap.width;
        let height = envmap.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/metallic_glass_envmap.wgsl"),
                "cs_main",
                "node.metallic_glass_envmap",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[GpuBinding::Texture {
                binding: 0,
                texture: envmap,
            }],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.metallic_glass_envmap",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn metallic_glass_envmap_declares_zero_inputs_and_texture_2d_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(MetallicGlassEnvmap::TYPE_ID, "node.metallic_glass_envmap");
        assert!(MetallicGlassEnvmap::INPUTS.is_empty());
        assert_eq!(MetallicGlassEnvmap::OUTPUTS.len(), 1);
        assert_eq!(MetallicGlassEnvmap::OUTPUTS[0].name, "envmap");
        assert_eq!(MetallicGlassEnvmap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn metallic_glass_envmap_has_no_params() {
        assert!(MetallicGlassEnvmap::PARAMS.is_empty());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MetallicGlassEnvmap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.metallic_glass_envmap");
    }
}
