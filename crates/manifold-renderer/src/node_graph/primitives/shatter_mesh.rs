//! `node.shatter_mesh` — per-triangle face-normal explosion of an `Array<MeshVertex>`.
//!
//! Flat triangle-list convention: triangle `t` reads/writes verts `[3t, 3t+3)`.
//! Each vertex in the triangle is displaced along the computed face normal by
//! `amount * hash(tri_id)`; output normals are set to that face normal. Trailing
//! partial triangles pass through unchanged.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amount`,
/// `seed` f32), then the derived `weights_len` (u32), then the codegen-injected
/// `dispatch_count`, padded to a 16-byte multiple. 4 words = 16 bytes. Matches
/// `standalone_for_spec::<ShatterMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShatterUniforms {
    amount: f32,
    seed: f32,
    weights_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ShatterMesh,
    type_id: "node.shatter_mesh",
    purpose: "Per-triangle face-normal explosion of an Array<MeshVertex> flat triangle list. Triangle t reads/writes verts [3t, 3t+3); each vertex is displaced along the computed face normal by amount * hash(tri_id + seed), and output normals are set to that face normal. `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Trailing partial triangles pass through unchanged. The flat-list convention matches node.facet_normals and node.spawn_from_mesh.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amount: ScalarF32 optional,
        seed: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'explode / shatter' deformer: every triangle drifts away from its neighbors along its own face normal, turning a smooth model into faceted shards. Wire node.mesh_ramp's `weights` output to grow the shatter progressively across the mesh. Because this atom recomputes face normals, it is naturally a fusion boundary — chain it with other deformers via the graph, but expect it to stand alone in the compiled kernel.",
    examples: [],
    picker: { label: "Shatter", category: Atom },
    summary: "Explodes a mesh into separate triangular shards, each sliding away along its own flat face normal.",
    category: Geometry3D,
    role: Filter,
    aliases: ["shatter", "shatter mesh", "explode", "shard"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/shatter_mesh_body.wgsl"),
    // `in` is BufferGather so the body can read the other two vertices of its
    // triangle; `weights` stays COINCIDENT. `weights_len` is a frame-derived
    // uniform the body uses to degrade past the buffer.
    input_access: [BufferGather],
    derived_uniforms: ["weights_len:u32"],
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for ShatterMesh {
    /// Output `out` is sized to match input `in` — shatter is a per-vertex-slot
    /// transform, no expansion.
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
        let amount = ctx.scalar_or_param("amount", 0.0);
        let seed = ctx.scalar_or_param("seed", 0.0);

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path: the runtime kernel is generated from `wgsl_body`
            // (with noise_common prepended) so this atom stays on the freeze
            // path. Bindings: uniform(0), buf_in(1), buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.shatter_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.shatter_mesh",
            )
        });

        let uniforms = ShatterUniforms {
            amount,
            seed,
            weights_len,
            dispatch_count: count,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.shatter_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn shatter_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(ShatterMesh::TYPE_ID, "node.shatter_mesh");

        let in_port = ShatterMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = ShatterMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["amount", "seed"] {
            let port = ShatterMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(ShatterMesh::OUTPUTS.len(), 1);
        assert_eq!(ShatterMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn shatter_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ShatterMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ShatterMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.shatter_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests against a CPU-computed expected output.
    use super::*;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
        }
    }

    /// The generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<ShatterMesh>()
            .expect("shatter_mesh buffer codegen")
    }

    fn cpu_face_normal(v0: [f32; 3], v1: [f32; 3], v2: [f32; 3]) -> [f32; 3] {
        let a = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let b = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
        let cx = a[1] * b[2] - a[2] * b[1];
        let cy = a[2] * b[0] - a[0] * b[2];
        let cz = a[0] * b[1] - a[1] * b[0];
        let len = (cx * cx + cy * cy + cz * cz).sqrt();
        if len == 0.0 {
            return [0.0, 0.0, 1.0];
        }
        [cx / len, cy / len, cz / len]
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_shatter(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        amount: f32,
        seed: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "shatter-mesh-test",
        );
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

        let (wbuf, weights_len) = match weights {
            Some(w) => {
                let mut padded = vec![0.0f32; src.len()];
                padded[..w.len().min(src.len())].copy_from_slice(&w[..w.len().min(src.len())]);
                let b = device.create_buffer_shared((padded.len() * 4).max(4) as u64);
                unsafe {
                    b.write(0, bytemuck::cast_slice(&padded));
                }
                (b, weights_len_override.unwrap_or(w.len() as u32))
            }
            None => (device.create_buffer_shared(std::mem::size_of_val(src) as u64), 0),
        };

        let uniforms = ShatterUniforms {
            amount,
            seed,
            weights_len,
            dispatch_count: src.len() as u32,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("shatter-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "shatter-mesh-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn amount_zero_is_identity() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
        ];

        let out = dispatch_shatter(&device, &gen_wgsl, &src, None, None, 0.0, 0.0);

        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].position, src[i].position, "amount=0 must be identity pos {i}");
            assert_eq!(out[i].uv, src[i].uv, "amount=0 must preserve uv {i}");
            // shatter_mesh always recomputes face normals, so normal identity is
            // not expected; face_normals_match_cpu_reference covers normal output.
        }
    }

    #[test]
    fn face_normals_match_cpu_reference() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [4.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 3.0, 0.0];
        let src = vec![
            mk_vertex(v0, [1.0, 0.0, 0.0], [0.0, 0.0]),
            mk_vertex(v1, [0.0, 1.0, 0.0], [1.0, 0.0]),
            mk_vertex(v2, [0.0, 0.0, 1.0], [0.0, 1.0]),
        ];

        let out = dispatch_shatter(&device, &gen_wgsl, &src, None, None, 1.0, 0.0);

        let expected_n = cpu_face_normal(v0, v1, v2);
        for i in 0..3 {
            assert!(
                (out[i].normal[0] - expected_n[0]).abs() < 1e-5,
                "vertex {i} normal.x: got {} expected {}",
                out[i].normal[0],
                expected_n[0]
            );
            assert!(
                (out[i].normal[1] - expected_n[1]).abs() < 1e-5,
                "vertex {i} normal.y: got {} expected {}",
                out[i].normal[1],
                expected_n[1]
            );
            assert!(
                (out[i].normal[2] - expected_n[2]).abs() < 1e-5,
                "vertex {i} normal.z: got {} expected {}",
                out[i].normal[2],
                expected_n[2]
            );
        }
    }

    #[test]
    fn displacement_matches_cpu_reference() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [1.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 1.0, 0.0];
        let src = vec![
            mk_vertex(v0, [0.0, 0.0, 1.0], [0.0, 0.0]),
            mk_vertex(v1, [0.0, 0.0, 1.0], [1.0, 0.0]),
            mk_vertex(v2, [0.0, 0.0, 1.0], [0.0, 1.0]),
        ];

        let amount = 0.5f32;
        let seed = 0.0f32;
        let out = dispatch_shatter(&device, &gen_wgsl, &src, None, None, amount, seed);

        let n = cpu_face_normal(v0, v1, v2);
        // Compute the expected hash value locally by evaluating hash_u32(0).
        let tri_id = 0u32;
        let key = tri_id.wrapping_add(seed.to_bits());
        let mut x = key;
        x ^= x >> 16;
        x = x.wrapping_mul(0x45d9f3b_u32);
        x ^= x >> 16;
        x = x.wrapping_mul(0x45d9f3b_u32);
        x ^= x >> 16;
        let h = x as f32 / 4294967295.0;

        let expected_pos = [
            v0[0] + n[0] * amount * h,
            v0[1] + n[1] * amount * h,
            v0[2] + n[2] * amount * h,
        ];
        assert!(
            (out[0].position[0] - expected_pos[0]).abs() < 1e-5,
            "vertex 0 pos.x: got {} expected {}",
            out[0].position[0],
            expected_pos[0]
        );
        assert!(
            (out[0].position[1] - expected_pos[1]).abs() < 1e-5,
            "vertex 0 pos.y: got {} expected {}",
            out[0].position[1],
            expected_pos[1]
        );
        assert!(
            (out[0].position[2] - expected_pos[2]).abs() < 1e-5,
            "vertex 0 pos.z: got {} expected {}",
            out[0].position[2],
            expected_pos[2]
        );
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [1.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 1.0, 0.0];
        let src: Vec<MeshVertex> = (0..12)
            .map(|i| {
                let v = match i % 3 {
                    0 => v0,
                    1 => v1,
                    _ => v2,
                };
                mk_vertex(v, [0.0, 0.0, 1.0], [0.0, 0.0])
            })
            .collect();
        let weights = [0.0f32, 0.0, 0.0]; // one full triangle (verts 0-2) weight 0

        let out = dispatch_shatter(&device, &gen_wgsl, &src, Some(&weights), Some(3), 1.0, 0.0);

        // First triangle should not move because all three weights are 0.
        for i in 0..3 {
            assert!(
                (out[i].position[0] - src[i].position[0]).abs() < 1e-5,
                "vertex {i} has weight 0 -> unchanged x"
            );
            assert!(
                (out[i].position[1] - src[i].position[1]).abs() < 1e-5,
                "vertex {i} has weight 0 -> unchanged y"
            );
            assert!(
                (out[i].position[2] - src[i].position[2]).abs() < 1e-5,
                "vertex {i} has weight 0 -> unchanged z"
            );
        }
        // Remaining triangles should degrade to w=1.0 and move.
        let tail_moved = out.iter().skip(3).enumerate().any(|(i, v)| {
            v.position[0] != src[i + 3].position[0]
                || v.position[1] != src[i + 3].position[1]
                || v.position[2] != src[i + 3].position[2]
        });
        assert!(tail_moved, "tail past weights_len should degrade to w=1.0 and displace");
    }
}
