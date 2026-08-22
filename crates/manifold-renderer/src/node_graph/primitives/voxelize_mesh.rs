//! `node.voxelize_mesh` — per-vertex voxel snap of an `Array<MeshVertex>`.
//!
//! Per vertex: `mix(pos, round(pos/cell_size) * cell_size, amount * w)`,
//! where `w` is the optional per-vertex `weights` input (degrading to 1.0
//! past a short/unwired buffer per the shipped deformer convention).
//! Normals, uv, and tangent pass through unchanged.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amount`,
/// `cell_size` f32), then the derived `weights_len` (u32), then the codegen-
/// injected `dispatch_count`, padded to a 16-byte multiple. 4 words = 16 bytes.
/// Matches `standalone_for_spec::<VoxelizeMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoxelizeUniforms {
    amount: f32,
    cell_size: f32,
    weights_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: VoxelizeMesh,
    type_id: "node.voxelize_mesh",
    purpose: "Per-vertex voxel snap of an Array<MeshVertex>. mix(pos, round(pos/cell_size)*cell_size, amount*w). `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Normals, uv, and tangent pass through unchanged — wire node.facet_normals downstream after a heavy voxelize if the unchanged normals start reading wrong under lighting.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amount: ScalarF32 optional,
        cell_size: ScalarF32 optional,
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
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cell_size"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.001, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'pixel-crush / voxelize' deformer. Ride `cell_size` down to zero mid-performance to snap the model into larger and larger voxels. Wire node.mesh_ramp's `weights` output to grow the voxelization progressively across the mesh instead of uniformly.",
    examples: [],
    picker: { label: "Voxelize", category: Atom },
    summary: "Snaps every vertex to a regular voxel grid, pixel-crushing a smooth mesh into chunky blocks.",
    category: Geometry3D,
    role: Filter,
    aliases: ["voxelize", "voxelize mesh", "pixel crush", "blockify"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/voxelize_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so it can chain with other mesh deformers in one
    // dispatch. `weights_len` is a frame-derived uniform the body uses to
    // bounds-check the coincident weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for VoxelizeMesh {
    /// Output `out` is sized to match input `in` — voxelization is a
    /// per-vertex transform, no expansion.
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
        let cell_size = ctx.scalar_or_param("cell_size", 1.0);

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
            // so this atom stays pointwise/fusable in the graph compiler.
            // Bindings: uniform(0), buf_in(1), buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.voxelize_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.voxelize_mesh",
            )
        });

        let uniforms = VoxelizeUniforms {
            amount,
            cell_size,
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
            "node.voxelize_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn voxelize_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(VoxelizeMesh::TYPE_ID, "node.voxelize_mesh");

        let in_port = VoxelizeMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = VoxelizeMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["amount", "cell_size"] {
            let port = VoxelizeMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(VoxelizeMesh::OUTPUTS.len(), 1);
        assert_eq!(VoxelizeMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn voxelize_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = VoxelizeMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = VoxelizeMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.voxelize_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Parity is against a hand-written Rust
    //! reference of the committed formula, element-wise, per
    //! DECOMPOSING_GENERATORS.md section 9.
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<VoxelizeMesh>()
            .expect("voxelize_mesh buffer codegen")
    }

    /// Hand reference: bit-for-bit the committed formula, f64 internally for
    /// a tighter analytic bar, cast to f32.
    fn expected_voxelize(pos: [f32; 3], amount: f32, cell_size: f32, w: f32) -> [f32; 3] {
        let cs = cell_size.max(1e-6);
        let voxel = [
            (pos[0] as f64 / cs as f64).round() as f32 * cs,
            (pos[1] as f64 / cs as f64).round() as f32 * cs,
            (pos[2] as f64 / cs as f64).round() as f32 * cs,
        ];
        [
            pos[0] + (voxel[0] - pos[0]) * amount * w,
            pos[1] + (voxel[1] - pos[1]) * amount * w,
            pos[2] + (voxel[2] - pos[2]) * amount * w,
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_voxelize(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        amount: f32,
        cell_size: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "voxelize-mesh-test",
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

        let uniforms = VoxelizeUniforms {
            amount,
            cell_size,
            weights_len,
            dispatch_count: src.len() as u32,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("voxelize-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "voxelize-mesh-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn matches_hand_formula_with_weights() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.2]),
            mk_vertex([1.3, -0.7, 2.4], [1.0, 0.0, 0.0], [0.3, 0.4]),
            mk_vertex([-0.6, 1.9, -1.1], [0.0, 0.0, 1.0], [0.5, 0.6]),
        ];
        let weights = [0.0f32, 0.5, 1.0];
        let amount = 1.0f32;
        let cell_size = 1.0f32;

        let out = dispatch_voxelize(&device, &gen_wgsl, &src, Some(&weights), None, amount, cell_size);

        for i in 0..src.len() {
            let exp = expected_voxelize(src[i].position, amount, cell_size, weights[i]);
            for c in 0..3 {
                assert!(
                    (out[i].position[c] - exp[c]).abs() < 1e-5,
                    "vertex {i} position[{c}]: got={} expected={exp:?}",
                    out[i].position[c]
                );
            }
            assert_eq!(out[i].normal, src[i].normal, "normal passes through");
            assert_eq!(out[i].uv, src[i].uv, "uv passes through");
        }
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

        let out = dispatch_voxelize(&device, &gen_wgsl, &src, None, None, 0.0, 1.0);

        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].position, src[i].position, "amount=0 must be identity pos {i}");
            assert_eq!(out[i].normal, src[i].normal, "amount=0 must preserve normal {i}");
            assert_eq!(out[i].uv, src[i].uv, "amount=0 must preserve uv {i}");
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // Step 0.11 to avoid WGSL round-to-even tie cases (e.g. 0.5), which
        // differ from Rust f64::round's half-away-from-zero.
        let src: Vec<MeshVertex> = (0..12)
            .map(|i| mk_vertex([i as f32 * 0.11, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0];
        let amount = 1.0f32;
        let cell_size = 1.0f32;

        let out = dispatch_voxelize(&device, &gen_wgsl, &src, Some(&weights), Some(2), amount, cell_size);

        assert!(
            (out[0].position[0] - src[0].position[0]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged"
        );
        assert!(
            (out[1].position[0] - src[1].position[0]).abs() < 1e-5,
            "vertex 1 has explicit weight 0 -> unchanged"
        );
        for (i, v) in out.iter().enumerate().skip(2).take(10) {
            let exp = expected_voxelize(src[i].position, amount, cell_size, 1.0);
            assert!(
                (v.position[0] - exp[0]).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0, got {} expected {}",
                v.position[0],
                exp[0]
            );
        }
    }
}
