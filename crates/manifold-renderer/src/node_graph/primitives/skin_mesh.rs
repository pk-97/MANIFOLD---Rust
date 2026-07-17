//! `node.skin_mesh` — per-vertex linear-blend GPU skinning
//! (GLTF_ANIMATION_DESIGN.md A2, D2).
//!
//! Barrier-free pure per-element kernel: for each vertex, blend up to 4
//! joint matrices (`node.gltf_skeleton_pose`'s `Array(JointMatrix)`
//! output, one skin matrix per joint — `jointWorldMatrix *
//! inverseBindMatrix`, computed CPU-side per frame) by that vertex's
//! per-vertex weights, per the glTF spec's linear-blend-skinning formula:
//!
//! `pos' = sum_k(weight[k] * (matrices[joints[k]] * vec4(pos, 1))).xyz`
//! `normal' = normalize(sum_k(weight[k] * (matrices[joints[k]] * vec4(normal, 0))).xyz)`
//!
//! `joints`/`weights` are COINCIDENT per-vertex inputs (same shape
//! `node.morph_mesh`'s `weights` input already proves for a per-vertex
//! side-channel — `Array(Vec4Vertex)`, reusing that existing 4-float
//! KnownItem rather than adding a joints/weights-specific type). The
//! joint-matrix palette is NOT coincident with the per-vertex dispatch —
//! it's looked up by index, so it's declared `BufferGather` (the same
//! access kind `node.neighbor_smooth`/`node.tube_from_path` already use
//! for a body-computed-index buffer read) — this keeps the kernel
//! barrier-free and codegen-path per CLAUDE.md's standing rule.
//!
//! `weights` are normalized defensively inside the body (`w / max(sum(w),
//! eps)`) — the glTF spec requires WEIGHTS_0 to sum to 1.0, but real-world
//! exported assets aren't always exact, and normalizing costs nothing on
//! the already-barrier-free per-vertex path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{JointMatrix, MeshVertex, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `joint_count` param (Int -> i32),
/// then the derived `joints_len`/`weights_len`/`matrices_len` (u32 each),
/// then the codegen-injected `dispatch_count`, padded to a 16-byte
/// multiple. 5 words + 3 pad = 32 bytes. Matches
/// `standalone_for_spec::<SkinMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkinMeshUniforms {
    joint_count: i32,
    joints_len: u32,
    weights_len: u32,
    matrices_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: SkinMesh,
    type_id: "node.skin_mesh",
    purpose: "Per-vertex linear-blend GPU skinning: deforms Array(MeshVertex) `in` by up to 4 joint matrices per vertex, looked up from `matrices` (a joint-index palette, node.gltf_skeleton_pose's output) via the coincident per-vertex `joints`/`weights` (Array(Vec4Vertex), 4 joint indices + 4 weights per vertex). pos' = sum(weight[k] * (matrices[joints[k]] * vec4(pos,1))).xyz; normal' likewise with w=0. Weights are normalized defensively (sum may not be exactly 1.0 on every real asset). Barrier-free per-element kernel — the codegen path (fusable), never a fusion-boundary WGSL include.",
    inputs: {
        in: Array(MeshVertex) required,
        joints: Array(Vec4Vertex) required,
        weights: Array(Vec4Vertex) required,
        matrices: Array(JointMatrix) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("joint_count"),
            label: "Joint Count",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 512.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.gltf_skinned_mesh_source's vertices/joints/weights and node.gltf_skeleton_pose's joint_matrices into this node's matching inputs. `joint_count` must match the skeleton pose node's own `joint_count` param (gltf_import.rs sets both from the same GltfObjectSkin) — out-of-range joint indices clamp to the last valid joint rather than reading out of bounds.",
    examples: [],
    picker: { label: "Skin Mesh", category: Atom },
    summary: "Deforms an imported rigged mesh by its animated skeleton — the GPU counterpart to a Skeleton Pose node's joint matrices.",
    category: Geometry3D,
    role: Filter,
    aliases: ["skin mesh", "skinning", "rig deform", "joint blend"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/skin_mesh_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident, BufferGather],
    derived_uniforms: ["joints_len:u32", "weights_len:u32", "matrices_len:u32"],
}

impl Primitive for SkinMesh {
    /// Output `out` follows `in`'s capacity — skinning is a pure
    /// per-vertex deform, same count in and out.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let joint_count = match ctx.params.get("joint_count") {
            Some(ParamValue::Float(f)) => f.round().clamp(0.0, 512.0) as i32,
            _ => 0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(joints_buf) = ctx.inputs.array("joints") else {
            return;
        };
        let Some(weights_buf) = ctx.inputs.array("weights") else {
            return;
        };
        let Some(matrices_buf) = ctx.inputs.array("matrices") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let vec4_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let matrix_size = std::mem::size_of::<JointMatrix>() as u64;

        let in_cap = (in_buf.size / vertex_size) as u32;
        let out_cap = (out_buf.size / vertex_size) as u32;
        let count = in_cap.min(out_cap);
        if count == 0 || joint_count == 0 {
            return;
        }
        let joints_len = (joints_buf.size / vec4_size) as u32;
        let weights_len = (weights_buf.size / vec4_size) as u32;
        let matrices_len = (matrices_buf.size / matrix_size) as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.skin_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.skin_mesh",
            )
        });

        let uniforms = SkinMeshUniforms {
            joint_count,
            joints_len,
            weights_len,
            matrices_len,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer { binding: 1, buffer: in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: joints_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: weights_buf, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: matrices_buf, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out_buf, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.skin_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn skin_mesh_declares_four_required_array_inputs_and_one_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let vec4_layout = ArrayType::of_known::<Vec4Vertex>();
        let matrix_layout = ArrayType::of_known::<JointMatrix>();

        assert_eq!(SkinMesh::TYPE_ID, "node.skin_mesh");
        assert_eq!(SkinMesh::INPUTS.len(), 4);
        let in_port = SkinMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));
        for name in ["joints", "weights"] {
            let port = SkinMesh::INPUTS.iter().find(|p| p.name == name).unwrap();
            assert!(port.required);
            assert_eq!(port.ty, PortType::Array(vec4_layout));
        }
        let matrices_port = SkinMesh::INPUTS.iter().find(|p| p.name == "matrices").unwrap();
        assert!(matrices_port.required);
        assert_eq!(matrices_port.ty, PortType::Array(matrix_layout));

        assert_eq!(SkinMesh::OUTPUTS.len(), 1);
        assert_eq!(SkinMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn skin_mesh_output_follows_in_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = SkinMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 4000_u32), ("joints", 4000_u32), ("weights", 4000_u32), ("matrices", 64_u32)];
        assert_eq!(Primitive::array_output_capacity(&prim, "out", &params, &inputs), Some(4000));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SkinMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.skin_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. No legacy predecessor to diff against —
    //! parity is against a hand-written Rust reference of the committed
    //! linear-blend-skinning formula, element-wise, per
    //! DECOMPOSING_GENERATORS.md §9.
    use super::*;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal, _pad1: 0.0, uv: [0.0, 0.0], _pad2: [0.0, 0.0] }
    }

    fn identity_matrix() -> JointMatrix {
        JointMatrix {
            c0: [1.0, 0.0, 0.0, 0.0],
            c1: [0.0, 1.0, 0.0, 0.0],
            c2: [0.0, 0.0, 1.0, 0.0],
            c3: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn translate_matrix(t: [f32; 3]) -> JointMatrix {
        JointMatrix {
            c0: [1.0, 0.0, 0.0, 0.0],
            c1: [0.0, 1.0, 0.0, 0.0],
            c2: [0.0, 0.0, 1.0, 0.0],
            c3: [t[0], t[1], t[2], 1.0],
        }
    }

    /// Generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<SkinMesh>()
            .expect("skin_mesh buffer codegen")
    }

    /// Hand Rust reference: bit-for-bit the committed formula (module doc
    /// comment) — sum(weight[k] * (M[joints[k]] * v)), normalized weights.
    fn expected_skin(
        v: &MeshVertex,
        joints: [f32; 4],
        weights: [f32; 4],
        matrices: &[JointMatrix],
    ) -> ([f32; 3], [f32; 3]) {
        let wsum = (weights[0] + weights[1] + weights[2] + weights[3]).max(1e-8);
        let mut pos = [0.0f32; 3];
        let mut nrm = [0.0f32; 3];
        for k in 0..4 {
            let w = weights[k] / wsum;
            let j = (joints[k].round() as usize).min(matrices.len() - 1);
            let m = &matrices[j];
            let mp = |c0: [f32; 4], c1: [f32; 4], c2: [f32; 4], c3: [f32; 4], p: [f32; 3], is_point: f32| -> [f32; 3] {
                [
                    c0[0] * p[0] + c1[0] * p[1] + c2[0] * p[2] + c3[0] * is_point,
                    c0[1] * p[0] + c1[1] * p[1] + c2[1] * p[2] + c3[1] * is_point,
                    c0[2] * p[0] + c1[2] * p[1] + c2[2] * p[2] + c3[2] * is_point,
                ]
            };
            let tp = mp(m.c0, m.c1, m.c2, m.c3, v.position, 1.0);
            let tn = mp(m.c0, m.c1, m.c2, m.c3, v.normal, 0.0);
            for c in 0..3 {
                pos[c] += w * tp[c];
                nrm[c] += w * tn[c];
            }
        }
        let mag = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt().max(1e-12);
        (pos, [nrm[0] / mag, nrm[1] / mag, nrm[2] / mag])
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_skin(
        device: &manifold_gpu::GpuDevice,
        verts: &[MeshVertex],
        joints: &[[f32; 4]],
        weights: &[[f32; 4]],
        matrices: &[JointMatrix],
        joint_count: i32,
    ) -> Vec<MeshVertex> {
        let wgsl = generated_wgsl();
        let pipeline =
            device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "skin-mesh-test");
        let in_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(verts));
        }
        let joints_v4: Vec<Vec4Vertex> = joints.iter().map(|j| Vec4Vertex { position: *j }).collect();
        let joints_buf = device.create_buffer_shared(std::mem::size_of_val(joints_v4.as_slice()) as u64);
        unsafe {
            joints_buf.write(0, bytemuck::cast_slice(&joints_v4));
        }
        let weights_v4: Vec<Vec4Vertex> = weights.iter().map(|w| Vec4Vertex { position: *w }).collect();
        let weights_buf = device.create_buffer_shared(std::mem::size_of_val(weights_v4.as_slice()) as u64);
        unsafe {
            weights_buf.write(0, bytemuck::cast_slice(&weights_v4));
        }
        let matrices_buf = device.create_buffer_shared(std::mem::size_of_val(matrices) as u64);
        unsafe {
            matrices_buf.write(0, bytemuck::cast_slice(matrices));
        }
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);

        let uniforms = SkinMeshUniforms {
            joint_count,
            joints_len: joints_v4.len() as u32,
            weights_len: weights_v4.len() as u32,
            matrices_len: matrices.len() as u32,
            dispatch_count: verts.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &joints_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &weights_buf, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &matrices_buf, offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: &out_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("skin-mesh-test");
        enc.dispatch_compute(&pipeline, &bindings, [(verts.len() as u32).div_ceil(256), 1, 1], "skin-mesh-test");
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, verts.len()) }.to_vec()
    }

    #[test]
    fn generated_matches_hand_formula_single_joint_full_weight() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        assert!(gen_wgsl.contains("struct Element"), "element struct synthesized");
        assert!(gen_wgsl.contains("var<storage, read_write>"), "output bound read_write");

        let verts = vec![
            mk_vertex([1.0, 2.0, 3.0], [0.0, 1.0, 0.0]),
            mk_vertex([-1.0, 0.0, 0.5], [1.0, 0.0, 0.0]),
        ];
        let joints = vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]];
        let weights = vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]];
        let matrices = vec![translate_matrix([5.0, 0.0, 0.0]), translate_matrix([0.0, 5.0, 0.0])];

        let out = dispatch_skin(&device, &verts, &joints, &weights, &matrices, 2);
        for i in 0..verts.len() {
            let (exp_pos, exp_n) = expected_skin(&verts[i], joints[i], weights[i], &matrices);
            for c in 0..3 {
                assert!(
                    (out[i].position[c] - exp_pos[c]).abs() < 1e-4,
                    "vertex {i} pos[{c}]: got={} expected={}",
                    out[i].position[c],
                    exp_pos[c]
                );
                assert!((out[i].normal[c] - exp_n[c]).abs() < 1e-4, "vertex {i} normal[{c}]");
            }
        }
    }

    #[test]
    fn generated_matches_hand_formula_two_joint_blend() {
        let device = crate::test_device();
        let verts = vec![mk_vertex([2.0, 0.0, 0.0], [0.0, 0.0, 1.0])];
        let joints = vec![[0.0, 1.0, 0.0, 0.0]];
        let weights = vec![[0.25, 0.75, 0.0, 0.0]];
        let matrices = vec![translate_matrix([10.0, 0.0, 0.0]), identity_matrix()];

        let out = dispatch_skin(&device, &verts, &joints, &weights, &matrices, 2);
        let (exp_pos, exp_n) = expected_skin(&verts[0], joints[0], weights[0], &matrices);
        for c in 0..3 {
            assert!(
                (out[0].position[c] - exp_pos[c]).abs() < 1e-4,
                "blended vertex pos[{c}]: got={} expected={}",
                out[0].position[c],
                exp_pos[c]
            );
            assert!((out[0].normal[c] - exp_n[c]).abs() < 1e-4, "blended vertex normal[{c}]");
        }
    }

    #[test]
    fn out_of_range_joint_index_clamps_to_last_valid_joint() {
        let device = crate::test_device();
        let verts = vec![mk_vertex([1.0, 0.0, 0.0], [0.0, 1.0, 0.0])];
        // Joint index 99 is out of range for a 1-joint palette — must
        // clamp to joint 0, not read out of bounds.
        let joints = vec![[99.0, 0.0, 0.0, 0.0]];
        let weights = vec![[1.0, 0.0, 0.0, 0.0]];
        let matrices = vec![translate_matrix([3.0, 0.0, 0.0])];

        let out = dispatch_skin(&device, &verts, &joints, &weights, &matrices, 1);
        assert!((out[0].position[0] - 4.0).abs() < 1e-4, "clamped to joint 0, got {}", out[0].position[0]);
    }
}
