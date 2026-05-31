//! `node.rotate_3d` — XYZ Euler rotation of an `Array<MeshVertex>`.
//!
//! WGSL port of `generators::generator_math::rotate_3d` — applies
//! rotations in X → Y → Z order to position and normal of each
//! vertex. Used by WireframeZoo-shaped graphs:
//! polytope_vertices → Rotate3D → (project) → render.

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
    purpose: "Apply XYZ Euler rotation to an Array<MeshVertex>. Rotates position and normal of each vertex in X → Y → Z order (matches generator_math::rotate_3d bit-for-bit). The 3D-equivalent of node.rotate_4d, used in WireframeZoo-shaped graphs: polytope_vertices → Rotate3D → (project) → render.",
    inputs: {
        in: Array(MeshVertex) required,
        // Port-shadows-param: when a wire is connected, the wired
        // value wins over the inline `angle_*` param. Lets the graph
        // drive angles from time / LFO / math nodes without lifting
        // each angle into a separate Value node.
        angle_x: ScalarF32 optional,
        angle_y: ScalarF32 optional,
        angle_z: ScalarF32 optional,
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
    summary: "Spins a 3D mesh around the X, Y, and Z axes. Wire an LFO or a beat into the angles to keep it turning.",
    category: Geometry3D,
    role: Filter,
    aliases: ["rotate 3d", "spin", "tumble", "euler"],
}

impl Primitive for Rotate3D {
    /// Output `out` is sized to match input `in` — rotation is a
    /// vertex-by-vertex transform.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle_x = ctx.scalar_or_param("angle_x", 0.0);
        let angle_y = ctx.scalar_or_param("angle_y", 0.0);
        let angle_z = ctx.scalar_or_param("angle_z", 0.0);

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
    fn rotate_3d_declares_mesh_in_and_three_optional_angle_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(Rotate3D::TYPE_ID, "node.rotate_3d");
        assert_eq!(Rotate3D::INPUTS.len(), 4);
        assert_eq!(Rotate3D::INPUTS[0].name, "in");
        assert!(Rotate3D::INPUTS[0].required);
        assert_eq!(Rotate3D::INPUTS[0].ty, PortType::Array(layout));
        for (i, name) in ["angle_x", "angle_y", "angle_z"].iter().enumerate() {
            assert_eq!(Rotate3D::INPUTS[i + 1].name, *name);
            assert!(!Rotate3D::INPUTS[i + 1].required);
            assert_eq!(Rotate3D::INPUTS[i + 1].ty, PortType::Scalar(ScalarType::F32));
        }
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
