//! `node.simplex_noise_2d` — 2D Simplex noise (Ashima Arts /
//! Stefan Gustavson / Ian McEwan).
//!
//! Pure generator. Output is the standard simplex noise remapped
//! to [0, 1] and broadcast to RGB (A = 1). Params: `scale`
//! (frequency in UV units — higher = finer cells), `offset_x` /
//! `offset_y` (pan; drive from an LFO to animate).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimplexUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    _pad: f32,
}

crate::primitive! {
    name: SimplexNoise2D,
    type_id: "node.simplex_noise_2d",
    purpose: "Pure generator. Classic 2D Simplex noise (Ashima Arts) remapped to [0, 1], broadcast to RGB (A = 1). The workhorse procedural noise for organic patterns, terrain, clouds, watercolor textures.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_x",
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_y",
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Drive offset_x / offset_y from an LFO or beat counter to animate. Scale governs cell size: ~1 = one cell across the image, ~32 = fine grain. Output is grayscale and pre-remapped to [0, 1]; chain into node.scale_offset_texture to recover signed noise, or into node.lut1d for color mapping. Pair with node.fbm_2d for richer multi-octave detail.",
    examples: [],
    picker: { label: "Simplex Noise 2D", category: Atom },
    summary: "Smooth random noise similar to Perlin but cleaner and with fewer directional artifacts. A good general-purpose noise.",
    category: Noise,
    role: Source,
    aliases: ["simplex", "noise", "Noise TOP"],
}

impl Primitive for SimplexNoise2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let offset_x = match ctx.params.get("offset_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let offset_y = match ctx.params.get("offset_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/simplex_noise_2d.wgsl"),
                "cs_main",
                "node.simplex_noise_2d",
            )
        });

        let uniforms = SimplexUniforms {
            scale,
            offset_x,
            offset_y,
            _pad: 0.0,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.simplex_noise_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn simplex_noise_2d_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(SimplexNoise2D::TYPE_ID, "node.simplex_noise_2d");
        assert!(SimplexNoise2D::INPUTS.is_empty());
        assert_eq!(SimplexNoise2D::OUTPUTS.len(), 1);
        assert_eq!(SimplexNoise2D::OUTPUTS[0].name, "out");
        assert_eq!(SimplexNoise2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn simplex_noise_2d_has_scale_offset_params() {
        let names: Vec<&str> = SimplexNoise2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["scale", "offset_x", "offset_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexNoise2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.simplex_noise_2d");
    }
}
