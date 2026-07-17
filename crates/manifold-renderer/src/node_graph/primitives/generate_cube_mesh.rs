//! `node.cube_mesh` — emit a unit cube as 36 triangle-list
//! `MeshVertex` entries (6 faces × 2 triangles × 3 vertices) with
//! per-face outward normals.
//!
//! Vertex data ported from
//! `generators/shaders/digital_plants_render.wgsl`'s hardcoded
//! cube constants. Pair with `node.render_copies` to
//! draw N copies of a cube under different transforms — the
//! decomposed shape of NestedCubes / DigitalPlants.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Number of triangle vertices in a cube mesh (6 faces × 2 triangles × 3 vertices).
/// Use this when sizing buffers for downstream consumers.
pub const CUBE_VERTEX_COUNT: u32 = 36;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`max_capacity` Int → i32 [allocation-only, the shader ignores it but it
/// occupies a uniform word], `size` f32) then the codegen-injected
/// `dispatch_count` (= output capacity, the guard), padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CubeUniforms {
    max_capacity: i32,
    size: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: GenerateCubeMesh,
    type_id: "node.cube_mesh",
    purpose: "Emit a unit cube as 36 triangle-list MeshVertex entries (6 faces × 2 triangles × 3 vertices) with per-face outward normals. The cube-shape building block for NestedCubes / DigitalPlants and any instanced-cube graph: pair with node.arrange_copies + node.render_copies to draw a field of cubes.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(36.0),
            range: Some((36.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "max_capacity is the chain-build pre-allocation ceiling — defaults to 36 (exactly one cube). Larger values pad the buffer with zero-vertex entries; useful only if downstream consumers expect a multi-mesh buffer. size scales the [-0.5, 0.5] unit cube. For non-cube wireframe shapes use node.platonic_solid_points + node.platonic_solid_edges.",
    examples: [],
    picker: { label: "Cube Mesh", category: Atom },
    summary: "Builds a unit cube as a 3D mesh ready to rotate, light, and render. The starting block for box-based geometry.",
    category: Geometry3D,
    role: Source,
    aliases: ["cube mesh", "generate cube mesh", "box", "cube", "Box SOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/generate_cube_mesh_body.wgsl"),
}

impl Primitive for GenerateCubeMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let size = match ctx.params.get("size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        // Allocation-only param — not used by the shader, but the generated
        // uniform lays out every PARAM, so pack it (the body ignores it).
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => CUBE_VERTEX_COUNT as i32,
        };

        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (dst.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer source
            // path; const cube tables inlined). generate_cube_mesh.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.cube_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.cube_mesh",
            )
        });

        let uniforms = CubeUniforms {
            max_capacity,
            size,
            dispatch_count: capacity,
            _pad0: 0,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.cube_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_cube_mesh_declares_zero_inputs_and_mesh_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(GenerateCubeMesh::TYPE_ID, "node.cube_mesh");
        assert!(GenerateCubeMesh::INPUTS.is_empty());
        assert_eq!(GenerateCubeMesh::OUTPUTS.len(), 1);
        assert_eq!(GenerateCubeMesh::OUTPUTS[0].name, "vertices");
        assert_eq!(
            GenerateCubeMesh::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn generate_cube_mesh_default_capacity_is_36() {
        let cap = GenerateCubeMesh::PARAMS
            .iter()
            .find(|p| p.name == "max_capacity")
            .unwrap();
        match cap.default {
            ParamValue::Float(n) => assert_eq!(n as u32, CUBE_VERTEX_COUNT),
            _ => panic!("expected Float (Int presentation hint)"),
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateCubeMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.cube_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain SOURCE parity oracle (freeze §12) — generate_cube_mesh had
    //! no GPU test. The generated kernel (const cube tables inlined in the body)
    //! must reproduce the hand kernel vertex-for-vertex, including the padding
    //! vertices past index 36. Compares position/normal/uv (not std430 padding).
    use super::*;

    fn dispatch_cube(wgsl: &str, capacity: u32, uniform: &[u8]) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "cube-oracle");
        let out_buf = device.create_buffer_shared(capacity as u64 * 48);
        let mut enc = device.create_encoder("cube-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(64), 1, 1],
            "cube-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice =
            unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, capacity as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_cube_mesh_matches_hand_kernel() {
        const CAPACITY: u32 = 40; // 36 cube verts + 4 padding
        let size = 2.5f32;

        // Hand layout: capacity(u32), size(f32), pad, pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&CAPACITY.to_le_bytes());
        hand.extend_from_slice(&size.to_le_bytes());
        hand.extend_from_slice(&[0u8; 8]);

        // Generated layout: max_capacity(i32), size(f32), dispatch_count(u32), pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&36i32.to_le_bytes());
        gen_bytes.extend_from_slice(&size.to_le_bytes());
        gen_bytes.extend_from_slice(&CAPACITY.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 4]);

        let hand_wgsl = include_str!("shaders/generate_cube_mesh.wgsl");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<GenerateCubeMesh>()
            .expect("generate_cube_mesh buffer codegen");

        let from_hand = dispatch_cube(hand_wgsl, CAPACITY, &hand);
        let from_gen = dispatch_cube(&gen_wgsl, CAPACITY, &gen_bytes);

        for i in 0..CAPACITY as usize {
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
            assert_eq!(from_hand[i].uv, from_gen[i].uv, "vertex {i} uv");
        }
    }
}
