//! `node.centered_uv` — UV recentered around 0 with per-axis scale.
//!
//! `out.r = (uv.x - 0.5) * scale_x`,
//! `out.g = (uv.y - 0.5) * scale_y`.
//!
//! The canonical "screen-centered, aspect-corrected" coordinate space
//! for any procedural pattern that wants to compose around screen
//! center. Replaces the value+math chain that an explicit
//! `(uv - 0.5) * (aspect, 1) * inverse_scale` decomposition would
//! otherwise need — every centered procedural reads from this one
//! primitive and slices out the channels it wants via `field_combine`,
//! `distance_to_point`, etc.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CenteredUvUniforms {
    scale_x: f32,
    scale_y: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: CenteredUv,
    type_id: "node.centered_uv",
    purpose: "UV recentered around 0 with per-axis scale. out.r = (uv.x - 0.5) * scale_x, out.g = (uv.y - 0.5) * scale_y. The canonical centered/aspect-corrected coordinate space for procedural patterns — replaces the explicit (uv - 0.5) * (aspect, 1) * inverse_scale chain a centered field would otherwise need.",
    inputs: {
        // Both scales port-shadowable so the typical Plasma-style
        // composition (`scale_x = aspect * inverse_scale`,
        // `scale_y = inverse_scale`) can be driven from upstream
        // Math nodes each frame.
        scale_x: ScalarF32 optional,
        scale_y: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale_x",
            label: "Scale X",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale_y",
            label: "Scale Y",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Pairs naturally with node.field_combine to slice X / Y / X+Y projections out of the centered space (a=1 b=0 for X, a=0 b=1 for Y, a=1 b=1 for X+Y), and with node.distance_to_point (cx=0 cy=0) for the radial projection. For aspect-correct patterns, wire `scale_x` from a Math node that multiplies aspect by an inverse-scale.",
    examples: [],
    picker: { label: "Centered UV", category: Atom },
}

impl Primitive for CenteredUv {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale_x = match ctx.inputs.scalar("scale_x") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("scale_x") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        let scale_y = match ctx.inputs.scalar("scale_y") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("scale_y") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/centered_uv.wgsl"),
                "cs_main",
                "node.centered_uv",
            )
        });

        let uniforms = CenteredUvUniforms {
            scale_x,
            scale_y,
            _pad0: 0.0,
            _pad1: 0.0,
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
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.centered_uv",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn centered_uv_declares_two_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(CenteredUv::TYPE_ID, "node.centered_uv");
        let ins = CenteredUv::INPUTS;
        assert_eq!(ins.len(), 2);
        assert_eq!(ins[0].name, "scale_x");
        assert!(!ins[0].required);
        assert_eq!(ins[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[1].name, "scale_y");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(CenteredUv::OUTPUTS.len(), 1);
        assert_eq!(CenteredUv::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn centered_uv_has_scale_x_and_scale_y_params() {
        let names: Vec<&str> = CenteredUv::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["scale_x", "scale_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CenteredUv::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.centered_uv");
    }
}
