//! `node.voronoi_2d` — 2D Worley/Voronoi cellular noise.
//!
//! Pure generator. Each integer cell holds one jittered feature
//! point. The shader returns F1 (distance to nearest), F2
//! (second-nearest), and F2 - F1 (cell-edge factor).
//!
//! Output:
//! - R = F1                (cell-center proximity)
//! - G = F2                (second-nearest distance)
//! - B = F2 - F1           (edge factor: high at cell boundaries)
//! - A = 1

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    jitter: f32,
    out_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Voronoi2D,
    type_id: "node.voronoi_2d",
    purpose: "Pure generator. 2D Worley / Voronoi cellular noise. Outputs F1 in R (distance to nearest feature point), F2 in G (second-nearest), F2-F1 in B (cell-edge factor — high at boundaries). Foundation for cellular patterns, cracked-glass, stained-glass, stars (sparse jitter), foam.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
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
        ParamDef {
            name: "jitter",
            label: "Jitter",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "out_scale",
            label: "Output Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "For star fields: chain into node.fract_texture → node.power_texture (high exponent ~16) to spike F1 into points. For cracked-glass / cell edges: read the B (F2-F1) channel via node.channel_mix. For watercolor patches: read R, threshold via node.threshold. Setting jitter to 0 gives a perfect grid; 1 gives full random cells.",
    examples: [],
    picker: { label: "Voronoi 2D", category: Atom },
}

impl Primitive for Voronoi2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 8.0,
        };
        let offset_x = match ctx.params.get("offset_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let offset_y = match ctx.params.get("offset_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let jitter = match ctx.params.get("jitter") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let out_scale = match ctx.params.get("out_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
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
                include_str!("shaders/voronoi_2d.wgsl"),
                "cs_main",
                "node.voronoi_2d",
            )
        });

        let uniforms = VoronoiUniforms {
            scale,
            offset_x,
            offset_y,
            jitter,
            out_scale,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
            "node.voronoi_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn voronoi_2d_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Voronoi2D::TYPE_ID, "node.voronoi_2d");
        assert!(Voronoi2D::INPUTS.is_empty());
        assert_eq!(Voronoi2D::OUTPUTS.len(), 1);
        assert_eq!(Voronoi2D::OUTPUTS[0].name, "out");
        assert_eq!(Voronoi2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn voronoi_2d_has_expected_params() {
        let names: Vec<&str> = Voronoi2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["scale", "offset_x", "offset_y", "jitter", "out_scale"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Voronoi2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.voronoi_2d");
    }
}
