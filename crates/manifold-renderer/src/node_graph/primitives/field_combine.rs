//! `node.field_combine` — per-pixel scalar field from `a*R + b*G + c`
//! over the input texture, broadcast to RGB (alpha = 1).
//!
//! The natural projection primitive for 2D coordinate fields: pair with
//! [`node.uv_field`](crate::node_graph::primitives::UvField) to extract
//! any linear combination of `(uv.x, uv.y)` as a scalar suitable for
//! [`node.sin_texture`](crate::node_graph::primitives::SinTexture) or
//! similar per-pixel math primitives. All three coefficients are
//! port-shadowable so per-frame transforms (rotation, aspect, scale)
//! can be derived through scalar `node.math` chains driven from
//! `system.generator_input.time`.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FieldCombineUniforms {
    a: f32,
    b: f32,
    c: f32,
    _pad: f32,
}

crate::primitive! {
    name: FieldCombine,
    type_id: "node.field_combine",
    purpose: "Per-pixel scalar field: out.rgb = a * in.r + b * in.g + c, alpha = 1. Projects a 2D coordinate texture (typically uv_field output) onto any linear combination of its channels, with an optional constant offset. All three coefficients (a, b, c) accept scalar wires for per-frame-derived transforms (rotation, aspect, scale).",
    inputs: {
        in: Texture2D required,
        a: ScalarF32 optional,
        b: ScalarF32 optional,
        c: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("a"),
            label: "R Coefficient",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("b"),
            label: "G Coefficient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("c"),
            label: "Constant Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Defaults (a=1, b=0, c=0) pass uv.x through unchanged. For Plasma-style rotated fields, wire `a` and `b` from node.math (Cos / Sin of time) and derive `c` to keep the field centered. Reads only R and G of the input — alpha and blue are ignored, alpha is forced to 1 on output.",
    examples: [],
    picker: { label: "Field Combine", category: Atom },
    summary: "Mixes the channels of a coordinate field into one value with weights and an offset. The math step that turns coordinates into a custom gradient.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["field combine", "channel mix", "linear combine"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/field_combine_body.wgsl"),
}

impl Primitive for FieldCombine {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let a = ctx.scalar_or_param("a", 1.0);
        let b = ctx.scalar_or_param("b", 0.0);
        let c = ctx.scalar_or_param("c", 0.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.field_combine standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.field_combine",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FieldCombineUniforms {
            a,
            b,
            c,
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
            "node.field_combine",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn field_combine_declares_required_texture_and_three_optional_scalar_inputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let ins = FieldCombine::INPUTS;
        assert_eq!(ins.len(), 4);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        for (port, expected_name) in ins.iter().skip(1).zip(["a", "b", "c"]) {
            assert_eq!(port.name, expected_name);
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(FieldCombine::OUTPUTS.len(), 1);
    }

    #[test]
    fn field_combine_registers_as_palette_atom() {
        let prim = FieldCombine::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.field_combine");
    }
}
