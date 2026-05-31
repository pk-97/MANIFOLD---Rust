//! `node.perlin_noise_2d` — classic 2D Perlin gradient noise.
//!
//! Pure generator. Uses the same wang-hash gradient table as
//! `node.flow_field_noise` for cross-primitive aesthetic
//! consistency. Output remapped to [0, 1] and broadcast to RGB
//! (A = 1).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PerlinUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    _pad: f32,
}

crate::primitive! {
    name: PerlinNoise2D,
    type_id: "node.perlin_noise_2d",
    purpose: "Pure generator. Classic 2D Perlin gradient noise, remapped to [0, 1] and broadcast to RGB (A = 1). Different aesthetic from node.simplex_noise_2d: square-grid artifacts at low scales, smoother lobes.",
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
    composition_notes: "Output is pre-remapped to [0, 1]; chain into node.scale_offset_texture (a=2, b=-1) to recover signed noise. Animate by driving offset_x / offset_y from an LFO. Same gradient table as node.flow_field_noise so layered compositions look coherent.",
    examples: [],
    picker: { label: "Perlin Noise 2D", category: Atom },
    summary: "Smooth, cloudy random noise, the classic for organic textures and slow-moving fields. Soft and rounded compared to other noise types.",
    category: Noise,
    role: Source,
    aliases: ["perlin", "noise", "clouds", "Noise TOP"],
}

impl Primitive for PerlinNoise2D {
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
                include_str!("shaders/perlin_noise_2d.wgsl"),
                "cs_main",
                "node.perlin_noise_2d",
            )
        });

        let uniforms = PerlinUniforms {
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
            "node.perlin_noise_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn perlin_noise_2d_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(PerlinNoise2D::TYPE_ID, "node.perlin_noise_2d");
        assert!(PerlinNoise2D::INPUTS.is_empty());
        assert_eq!(PerlinNoise2D::OUTPUTS.len(), 1);
        assert_eq!(PerlinNoise2D::OUTPUTS[0].name, "out");
        assert_eq!(PerlinNoise2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn perlin_noise_2d_has_scale_offset_params() {
        let names: Vec<&str> = PerlinNoise2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["scale", "offset_x", "offset_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PerlinNoise2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.perlin_noise_2d");
    }
}
