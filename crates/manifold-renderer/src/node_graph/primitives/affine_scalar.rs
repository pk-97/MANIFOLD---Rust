//! `node.scale_offset_value` — `out = a * scale + offset` on a single scalar
//! wire, with `scale` and `offset` as static params (and port-shadowable
//! for completeness).
//!
//! Replaces the `Value(k) + Math(Multiply) + Value(b) + Math(Add)`
//! cluster that scalar derivations like `3 + 5 * complexity`,
//! `0.3 - 0.28 * contrast`, or `t * 0.3` would otherwise need. One
//! node per affine remap instead of four.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

crate::primitive! {
    name: AffineScalar,
    type_id: "node.scale_offset_value",
    purpose: "Scalar affine remap: out = a * scale + offset. The scalar counterpart of node.scale_offset_image — collapses Value+Math+Value+Math derivations like `3 + 5*x` or `0.3 - 0.28*x` into a single node. All three inputs are port-shadows-param: an inline param value drives the op when the wire is unwired.",
    inputs: {
        a: ScalarF32 required,
        scale: ScalarF32 optional,
        offset: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("a"),
            label: "A",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset"),
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Use to remap one scalar to another via a single affine step. Defaults (scale=1, offset=0) pass through. For `freq = 3 + 5 * complexity`: scale=5, offset=3, a=complexity (wired or bound). For `t = time * speed`: scale=speed_value, offset=0 (or just use Math.Multiply if `scale` is itself a wire). Negative `scale` lets you express subtractions inline (e.g. `0.3 - 0.28*c` is scale=-0.28, offset=0.3).",
    examples: [],
    picker: { label: "Scale + Offset (value)", category: Driver },
    summary: "Multiplies a value by a scale and adds an offset, the everyday way to rescale a control signal into the range a knob wants. Set the scale negative to invert.",
    category: Control,
    role: Control,
    aliases: ["scale offset", "affine scalar", "rescale", "map range", "attenuvert"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for AffineScalar {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let a = ctx.scalar_or_param("a", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let offset = ctx.scalar_or_param("offset", 0.0);
        ctx.outputs
            .set_scalar("out", ParamValue::Float(a * scale + offset));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn affine_scalar_declares_a_required_and_scale_offset_optional() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(AffineScalar::TYPE_ID, "node.scale_offset_value");
        let ins = AffineScalar::INPUTS;
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "a");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[1].name, "scale");
        assert!(!ins[1].required);
        assert_eq!(ins[2].name, "offset");
        assert!(!ins[2].required);
        assert_eq!(AffineScalar::OUTPUTS.len(), 1);
        assert_eq!(AffineScalar::OUTPUTS[0].ty, PortType::Scalar(ScalarType::F32));
    }

    #[test]
    fn affine_scalar_has_a_scale_and_offset_params() {
        let names: Vec<&str> = AffineScalar::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["a", "scale", "offset"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = AffineScalar::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scale_offset_value");
    }
}
