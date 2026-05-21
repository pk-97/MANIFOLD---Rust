//! `node.rotate_3d` — XYZ Euler rotation of an `Array<MeshVertex>`.
//!
//! WGSL port of `generators::generator_math::rotate_3d` — applies
//! rotations in X → Y → Z order to position and normal of each
//! vertex. Used by WireframeZoo-shaped graphs:
//! GeneratePlatonicSolid → Rotate3D → (project) → render.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Rotate3DUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    angle_x: f32,
    angle_y: f32,
    angle_z: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Rotate3D,
    type_id: "node.rotate_3d",
    purpose: "Apply XYZ Euler rotation to an Array<MeshVertex>. Rotates position and normal of each vertex in X → Y → Z order (matches generator_math::rotate_3d bit-for-bit). The 3D-equivalent of node.rotate_4d, used in WireframeZoo-shaped graphs: GeneratePlatonicSolid → Rotate3D → (project) → render.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "angle_x",
            label: "Angle X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "angle_y",
            label: "Angle Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "angle_z",
            label: "Angle Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    composition_notes: "Active count = input buffer's vertex count (full pass-through; capacity-bound only). Output normals are rotated alongside positions so downstream rendering / lighting stays correct. For 4D rotation (Tesseract / Duocylinder) use node.rotate_4d.",
    examples: [],
    picker: { label: "Rotate 3D", category: Atom },
}

impl Primitive for Rotate3D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle_x = match ctx.params.get("angle_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let angle_y = match ctx.params.get("angle_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let angle_z = match ctx.params.get("angle_z") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / vertex_size) as u32;
        let active_count = capacity;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/rotate_3d.wgsl"),
                "cs_main",
                "node.rotate_3d",
            )
        });

        let uniforms = Rotate3DUniforms {
            active_count,
            capacity,
            _pad0: 0,
            _pad1: 0,
            angle_x,
            angle_y,
            angle_z,
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
            "node.rotate_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn rotate_3d_declares_mesh_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<MeshVertex>() as u32,
            item_align: std::mem::align_of::<MeshVertex>() as u32,
        };
        assert_eq!(Rotate3D::TYPE_ID, "node.rotate_3d");
        assert_eq!(Rotate3D::INPUTS.len(), 1);
        assert_eq!(Rotate3D::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(Rotate3D::OUTPUTS.len(), 1);
        assert_eq!(Rotate3D::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn rotate_3d_has_three_angle_params() {
        let names: Vec<&str> = Rotate3D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["angle_x", "angle_y", "angle_z"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Rotate3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotate_3d");
    }
}
