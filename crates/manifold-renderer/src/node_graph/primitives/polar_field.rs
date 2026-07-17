//! `node.polar_field` — per-pixel polar coordinates around a
//! configurable center. R = angle (normalized 0..1), G = radius
//! (UV distance), B = 0, A = 1.
//!
//! Companion to `node.distance_to_point` (which writes only the
//! scalar distance) and `node.uv_field` (Cartesian).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PolarUniforms {
    cx: f32,
    cy: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: PolarField,
    type_id: "node.polar_field",
    purpose: "Pure generator. Per-pixel polar coordinates around a configurable center: R = angle (atan2, normalized to [0,1]), G = radius (UV distance), B = 0, A = 1. Building block for spirals, rotations, kaleidoscopes, and any radial composition.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cx"),
            label: "Center X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cy"),
            label: "Center Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Angle is normalized so a full sweep is 0..1 (handy for direct lut1d / sin compositions). Pair node.polar_field → node.sin_texture for sector patterns, → node.wrap(scale) for repeating wedges, → node.compose multiplicatively with a radius mask for circular sectors.",
    examples: [],
    picker: { label: "Polar Field", category: Atom },
    summary: "Outputs each pixel's angle and distance from a centre instead of its X and Y. The base for spirals, tunnels, and kaleidoscopes.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["polar", "angle distance", "radial coordinates"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/polar_field_body.wgsl"),
}

impl Primitive for PolarField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = match ctx.params.get("cx") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let cy = match ctx.params.get("cy") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
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
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.polar_field standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.polar_field",
            )
        });

        let uniforms = PolarUniforms {
            cx,
            cy,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.polar_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn polar_field_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(PolarField::TYPE_ID, "node.polar_field");
        assert!(PolarField::INPUTS.is_empty());
        assert_eq!(PolarField::OUTPUTS.len(), 1);
        assert_eq!(PolarField::OUTPUTS[0].name, "out");
        assert_eq!(PolarField::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn polar_field_has_cx_cy_params() {
        let names: Vec<&str> = PolarField::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["cx", "cy"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PolarField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.polar_field");
    }
}
