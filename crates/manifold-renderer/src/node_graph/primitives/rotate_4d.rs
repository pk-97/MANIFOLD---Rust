//! `node.rotate_4d` — apply a 4D rotation (XY, ZW, XW planes) to
//! an `Array<Vec4Vertex>` stream.
//!
//! Phase B of `BUFFER_PORT_PLAN`. Mirrors
//! `generators::generator_math::rotate_4d` so the behaviour is
//! bit-identical to Tesseract / Duocylinder / WireframeZoo when
//! wired with the same base verts. The transform primitive in
//! the mesh-family triad: producer → Rotate4D → renderer.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RotateUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    angle_xy: f32,
    angle_zw: f32,
    angle_xw: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Rotate4D,
    type_id: "node.rotate_4d",
    purpose: "Apply 4D rotation (XY, ZW, XW planes) to an Array<Vec4Vertex>. Matches generator_math::rotate_4d bit-for-bit. The transform stage of the 4D wireframe pipeline: producer → Rotate4D → renderer.",
    inputs: {
        in: Array(Vec4Vertex) required,
    },
    outputs: {
        out: Array(Vec4Vertex),
    },
    params: [
        ParamDef {
            name: "angle_xy",
            label: "Angle XY",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
        ParamDef {
            name: "angle_zw",
            label: "Angle ZW",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
        ParamDef {
            name: "angle_xw",
            label: "Angle XW",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
    ],
    composition_notes: "Defaults match Tesseract's preset (0.6, 0.4, 0.25). Wire angle_* through Math nodes for time-based tumble. Active-count uses the *input* buffer's item count — output writes the same N items.",
    examples: [],
    picker: { label: "Rotate 4D", category: Atom },
}

impl Primitive for Rotate4D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle_xy = match ctx.params.get("angle_xy") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.6,
        };
        let angle_zw = match ctx.params.get("angle_zw") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.4,
        };
        let angle_xw = match ctx.params.get("angle_xw") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.25,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / item_size) as u32;
        let active_count = capacity;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/rotate_4d.wgsl"),
                "cs_main",
                "node.rotate_4d",
            )
        });

        let uniforms = RotateUniforms {
            active_count,
            capacity,
            _pad0: 0,
            _pad1: 0,
            angle_xy,
            angle_zw,
            angle_xw,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.rotate_4d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn rotate_4d_declares_vec4_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<Vec4Vertex>() as u32,
            item_align: std::mem::align_of::<Vec4Vertex>() as u32,
        };
        assert_eq!(Rotate4D::TYPE_ID, "node.rotate_4d");
        assert_eq!(Rotate4D::INPUTS.len(), 1);
        assert_eq!(Rotate4D::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(Rotate4D::OUTPUTS.len(), 1);
        assert_eq!(Rotate4D::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn rotate_4d_has_three_rotation_angles() {
        let names: Vec<&str> = Rotate4D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["angle_xy", "angle_zw", "angle_xw"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Rotate4D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotate_4d");
    }
}
