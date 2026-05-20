//! `node.distance_to_point` — per-pixel scalar Euclidean distance
//! from a configurable center in UV space.
//!
//! Output: distance written into R, G, and B (A = 1). Downstream
//! per-pixel math primitives can read the scalar from any channel.
//!
//! Companion to `node.uv_field` and `node.polar_field` in the
//! procedural texture math family.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DistanceUniforms {
    cx: f32,
    cy: f32,
    scale: f32,
    scale_x: f32,
    scale_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: DistanceToPoint,
    type_id: "node.distance_to_point",
    purpose: "Pure generator. Per-pixel Euclidean distance from a configurable center in UV space, broadcast to R/G/B (A=1). Building block for radial fields, circle fields, and tunnel-like compositions.",
    inputs: {
        // Port-shadows-param for all five scalar params: when a
        // scalar is wired, it overrides the static param each frame.
        // Lets generator graphs derive cx/cy from a Math node and
        // animate scale (or scale_x/scale_y for anisotropic length
        // fields) from the same complexity wire that feeds downstream
        // sin_texture frequencies.
        cx: ScalarF32 optional,
        cy: ScalarF32 optional,
        scale: ScalarF32 optional,
        scale_x: ScalarF32 optional,
        scale_y: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "cx",
            label: "Center X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cy",
            label: "Center Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale_x",
            label: "Scale X (anisotropic)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale_y",
            label: "Scale Y (anisotropic)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output range without scale: [0, ~1.414] (sqrt(2) at corners when center is opposite corner). Use scale to remap into a target range, or chain into node.scale_offset_texture for affine remap. Pair with node.sin_texture for concentric rings, node.lut1d for radial gradients, node.compose with a threshold for circle masks.",
    examples: [],
    picker: { label: "Distance to Point", category: Atom },
}

fn read_scalar(
    ctx: &EffectNodeContext<'_, '_>,
    name: &str,
    default: f32,
) -> f32 {
    match ctx.inputs.scalar(name) {
        Some(ParamValue::Float(f)) => f,
        _ => match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        },
    }
}

impl Primitive for DistanceToPoint {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = read_scalar(ctx, "cx", 0.5);
        let cy = read_scalar(ctx, "cy", 0.5);
        let scale = read_scalar(ctx, "scale", 1.0);
        let scale_x = read_scalar(ctx, "scale_x", 1.0);
        let scale_y = read_scalar(ctx, "scale_y", 1.0);

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
                include_str!("shaders/distance_to_point.wgsl"),
                "cs_main",
                "node.distance_to_point",
            )
        });

        let uniforms = DistanceUniforms {
            cx,
            cy,
            scale,
            scale_x,
            scale_y,
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
            "node.distance_to_point",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn distance_to_point_declares_five_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(DistanceToPoint::TYPE_ID, "node.distance_to_point");
        let ins = DistanceToPoint::INPUTS;
        assert_eq!(ins.len(), 5);
        let names: Vec<&str> = ins.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["cx", "cy", "scale", "scale_x", "scale_y"]);
        for port in ins {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(DistanceToPoint::OUTPUTS.len(), 1);
        assert_eq!(DistanceToPoint::OUTPUTS[0].name, "out");
        assert_eq!(DistanceToPoint::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn distance_to_point_has_all_five_params() {
        let names: Vec<&str> = DistanceToPoint::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["cx", "cy", "scale", "scale_x", "scale_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DistanceToPoint::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.distance_to_point");
    }
}
