//! `node.rotate_coordinates` — rotate a coordinate field by an angle.
//!
//! Reads (x, y) from the input's R/G channels and writes the rotated
//! (x', y') back to R/G:
//!
//! ```text
//! x' = x * cos(angle) - y * sin(angle)
//! y' = x * sin(angle) + y * cos(angle)
//! ```
//!
//! Operates on coordinate textures (output of `node.centered_uv`,
//! `node.uv_field`, etc.) — not pixel-sampled images. The whole
//! `angle → cos / sin / -sin → field_combine(a=cos, b=-sin)` chain
//! that an explicit rotation decomposition would require collapses
//! into this one primitive plus a downstream channel pick.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Rotate2DUniforms {
    angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Rotate2D,
    type_id: "node.rotate_coordinates",
    purpose: "Rotate a 2D coordinate field around the origin by `angle` (radians). Reads (x, y) from input R/G, writes rotated (x', y') back to R/G. Collapses the `angle → cos / sin / neg_sin → field_combine(cos, -sin)` chain that any rotated-projection effect would otherwise need.",
    inputs: {
        in: Texture2D required,
        // Port-shadowable for animation: drive `angle` from a time
        // wire times some rate.
        angle: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Use upstream of node.field_combine to extract a rotated coordinate channel as a scalar field (Plasma's v5 rotated-X term). Counter-clockwise: positive angle rotates +X toward +Y. Input must be a coordinate texture (centered_uv, uv_field, etc.) — the primitive does not resample image content.",
    examples: [],
    picker: { label: "Rotate Coordinates", category: Atom },
    summary: "Rotates a coordinate field around the centre. This spins the coordinates used to build a warp, not the image itself. For the picture, use Flip or a transform.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["rotate coordinates", "rotate 2d", "rotate field", "spin"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_2d_body.wgsl"),
}

impl Primitive for Rotate2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle = match ctx.inputs.scalar("angle") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("angle") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
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
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. The hand shader
            // (`shaders/rotate_2d.wgsl`, the parity oracle) was deleted
            // 2026-07-20 (W1-B, migration scaffolding retired).
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.rotate_coordinates standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rotate_coordinates",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = Rotate2DUniforms {
            angle,
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
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.rotate_coordinates",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn rotate_2d_declares_required_in_and_optional_angle() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(Rotate2D::TYPE_ID, "node.rotate_coordinates");
        let ins = Rotate2D::INPUTS;
        assert_eq!(ins.len(), 2);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "angle");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(Rotate2D::OUTPUTS.len(), 1);
        assert_eq!(Rotate2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn rotate_2d_has_angle_param() {
        let names: Vec<&str> = Rotate2D::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["angle"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Rotate2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotate_coordinates");
    }
}

