//! `node.rotate_3d` — XYZ Euler rotation of an `Array<MeshVertex>`.
//!
//! WGSL port of `generators::generator_math::rotate_3d` — applies
//! rotations in X → Y → Z order to position and normal of each
//! vertex. Used by Wireframe-shaped graphs:
//! polytope_vertices → Rotate3D → (project) → render.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the three Angle params (f32) in PARAMS
/// order, then the codegen-injected `dispatch_count` (= vertex capacity, the
/// guard). 4 words = 16 bytes. `active_count == capacity` (full pass), so no
/// inactive-collapse field is needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Rotate3DUniforms {
    angle_x: f32,
    angle_y: f32,
    angle_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: Rotate3D,
    type_id: "node.rotate_3d",
    purpose: "Apply XYZ Euler rotation to an Array<MeshVertex>. Rotates position and normal of each vertex in X → Y → Z order (matches generator_math::rotate_3d bit-for-bit). The 3D-equivalent of node.rotate_4d, used in Wireframe-shaped graphs: polytope_vertices → Rotate3D → (project) → render.",
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
            name: Cow::Borrowed("angle_x"),
            label: "Angle X",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_y"),
            label: "Angle Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_z"),
            label: "Angle Z",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Active count = input buffer's vertex count (full pass-through; capacity-bound only). Output normals are rotated alongside positions so downstream rendering / lighting stays correct. For 4D rotation (Tesseract / Duocylinder) use node.rotate_4d.",
    examples: [],
    picker: { label: "Rotate 3D", category: Atom },
    summary: "Spins a 3D mesh around the X, Y, and Z axes. Wire an LFO or a beat into the angles to keep it turning.",
    category: Geometry3D,
    role: Filter,
    aliases: ["rotate 3d", "spin", "tumble", "euler"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_3d_body.wgsl"),
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
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path). rotate_3d.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.rotate_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rotate_3d",
            )
        });
        let _ = active_count;

        let uniforms = Rotate3DUniforms {
            angle_x,
            angle_y,
            angle_z,
            dispatch_count: capacity,
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
            [capacity.div_ceil(256), 1, 1],
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
        let names: Vec<&str> = Rotate3D::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["angle_x", "angle_y", "angle_z"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Rotate3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotate_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain parity oracle (freeze §12) — rotate_3d had no GPU test
    //! before the cutover. The generated kernel must reproduce the hand
    //! `rotate_3d.wgsl` vertex-for-vertex: rotated position + normal, uv passed
    //! through. Trig is computed on-GPU both ways → bit-identical. Compares the
    //! meaningful fields (position/normal/uv), not the std430 padding.
    use super::*;

    fn mesh_vertex(pos: [f32; 3], norm: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal: norm, _pad1: 0.0, uv, _pad2: [0.0; 2] }
    }

    fn dispatch_rotate3d(wgsl: &str, verts: &[MeshVertex], uniform: &[u8]) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "rot3d-oracle");
        let n = verts.len() as u32;
        let in_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(verts));
        }
        let mut enc = device.create_encoder("rot3d-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out_buf, offset: 0 },
            ],
            [n.div_ceil(64), 1, 1],
            "rot3d-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, verts.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_rotate3d_matches_hand_kernel() {
        let verts = [
            mesh_vertex([0.3, 0.7, 0.5], [0.0, 0.0, 1.0], [0.1, 0.2]),
            mesh_vertex([-0.2, 0.4, 1.0], [1.0, 0.0, 0.0], [0.3, 0.4]),
            mesh_vertex([0.6, -0.5, -0.3], [0.0, 1.0, 0.0], [0.5, 0.6]),
            mesh_vertex([-0.4, -0.4, 0.8], [0.577, 0.577, 0.577], [0.7, 0.8]),
        ];
        let n = verts.len() as u32;
        let (ax, ay, az) = (0.5f32, 1.0f32, -0.7f32);

        // Hand layout: active_count(u32)=n, capacity(u32)=n, pad, pad, ax, ay, az, pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&[0u8; 8]);
        hand.extend_from_slice(&ax.to_le_bytes());
        hand.extend_from_slice(&ay.to_le_bytes());
        hand.extend_from_slice(&az.to_le_bytes());
        hand.extend_from_slice(&[0u8; 4]);

        // Generated layout: ax, ay, az, dispatch_count(u32).
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&ax.to_le_bytes());
        gen_bytes.extend_from_slice(&ay.to_le_bytes());
        gen_bytes.extend_from_slice(&az.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());

        let hand_wgsl = include_str!("shaders/rotate_3d.wgsl");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Rotate3D>()
            .expect("rotate_3d buffer codegen");

        let from_hand = dispatch_rotate3d(hand_wgsl, &verts, &hand);
        let from_gen = dispatch_rotate3d(&gen_wgsl, &verts, &gen_bytes);

        for i in 0..verts.len() {
            for c in 0..3 {
                assert!(
                    (from_hand[i].position[c] - from_gen[i].position[c]).abs() < 1e-6,
                    "vertex {i} position[{c}]: hand={} gen={}",
                    from_hand[i].position[c],
                    from_gen[i].position[c]
                );
                assert!(
                    (from_hand[i].normal[c] - from_gen[i].normal[c]).abs() < 1e-6,
                    "vertex {i} normal[{c}]"
                );
            }
            assert_eq!(from_hand[i].uv, from_gen[i].uv, "vertex {i} uv passthrough");
        }
    }
}
