//! `node.grid_mesh` — emit a regular NxM grid of
//! `MeshVertex` items laid out as a flat plane in XZ.
//!
//! Phase B of `BUFFER_PORT_PLAN`. First primitive in the mesh
//! family — zero inputs, one Array(MeshVertex) output. Params
//! drive grid resolution and world-space size; the chain build
//! pre-allocates `max_capacity` vertices and the runtime
//! initialises `resolution_x * resolution_y` of them per frame.
//!
//! Downstream pairing: feed into `node.render_mesh` for direct
//! rendering, or into a future `node.push_mesh` primitive
//! that perturbs Y by a Texture2D sample (the path that unlocks
//! MetallicGlass-style feedback-displacement on arbitrary
//! source textures).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`max_capacity` Int → i32 [allocation-only, the shader ignores it but it
/// occupies a uniform word], `resolution_x`/`resolution_y` Int → i32,
/// `size_x`/`size_y` f32) then the codegen-injected `dispatch_count` (=
/// output capacity, the guard), padded to 16 bytes. `origin_x`/`origin_z`
/// are always 0.0 in the hand kernel and are not params, so they're not
/// threaded through the generated uniform. 8 words = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    max_capacity: i32,
    resolution_x: i32,
    resolution_y: i32,
    size_x: f32,
    size_y: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GenerateGridMesh,
    type_id: "node.grid_mesh",
    purpose: "Emit a regular NxM grid of MeshVertex items in the XZ plane, sized in world units. Pair with a displacement primitive that perturbs Y from a Texture2D, then route to node.render_mesh. The unlock for MetallicGlass-shaped graphs where the displacement source is wire-controlled.",
    inputs: {
        size_x: ScalarF32 optional,
        size_y: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(2_097_152.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("resolution_x"),
            label: "Resolution X",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("resolution_y"),
            label: "Resolution Y",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size_x"),
            label: "Size X",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size_y"),
            label: "Size Y",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "max_capacity ≥ resolution_x × resolution_y. The chain build pre-allocates max_capacity × 32 bytes and triggers a rebuild when changed; resolution sliders only write uniforms. Default 256×256 = 65k vertices ≈ 2 MB. size_x / size_y are port-shadows-param: aspect-correct the mesh by wiring `system.generator_input.aspect → math.multiply(b=2.0) → size_x` (matches the legacy MetallicGlass mesh that spans [-aspect, +aspect] in X).",
    examples: [],
    picker: { label: "Grid Mesh", category: Atom },
    summary: "Builds a flat grid of points as a 3D mesh, the base for terrain, cloth, and displacement looks. Pair it with Surface Bumps or Push Mesh.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid mesh", "generate grid mesh", "plane", "terrain", "Grid SOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/generate_grid_mesh_body.wgsl"),
}

impl Primitive for GenerateGridMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => 2_097_152,
        };
        let resolution_x = match ctx.params.get("resolution_x") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let resolution_y = match ctx.params.get("resolution_y") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let size_x = ctx.scalar_or_param("size_x", 2.0);
        let size_y = ctx.scalar_or_param("size_y", 2.0);

        let Some(out_buf) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (out_buf.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // source path). generate_grid_mesh.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.grid_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.grid_mesh",
            )
        });

        let uniforms = GridUniforms {
            max_capacity,
            resolution_x: resolution_x as i32,
            resolution_y: resolution_y as i32,
            size_x,
            size_y,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.grid_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_grid_mesh_declares_size_inputs_and_one_mesh_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_vertex_layout = ArrayType::of_known::<MeshVertex>();

        assert_eq!(GenerateGridMesh::TYPE_ID, "node.grid_mesh");
        assert_eq!(GenerateGridMesh::INPUTS.len(), 2);
        assert_eq!(GenerateGridMesh::INPUTS[0].name, "size_x");
        assert!(!GenerateGridMesh::INPUTS[0].required);
        assert_eq!(GenerateGridMesh::INPUTS[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GenerateGridMesh::INPUTS[1].name, "size_y");
        assert!(!GenerateGridMesh::INPUTS[1].required);
        assert_eq!(GenerateGridMesh::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GenerateGridMesh::OUTPUTS.len(), 1);
        assert_eq!(GenerateGridMesh::OUTPUTS[0].name, "vertices");
        assert_eq!(
            GenerateGridMesh::OUTPUTS[0].ty,
            PortType::Array(mesh_vertex_layout)
        );
    }

    #[test]
    fn generate_grid_mesh_has_capacity_resolution_and_size_params() {
        let names: Vec<&str> = GenerateGridMesh::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "max_capacity",
                "resolution_x",
                "resolution_y",
                "size_x",
                "size_y",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateGridMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.grid_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain SOURCE parity oracle (freeze §12) — generate_grid_mesh had
    //! no GPU test. The generated kernel must reproduce the hand kernel
    //! vertex-for-vertex, including the zero-filled inactive slots past
    //! resolution_x * resolution_y.
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn dispatch_grid(wgsl: &str, capacity: u32, uniform: &[u8]) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "grid-oracle");
        let out_buf = device.create_buffer_shared(capacity as u64 * 48);
        let mut enc = device.create_encoder("grid-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(64), 1, 1],
            "grid-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice =
            unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, capacity as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_grid_mesh_matches_hand_kernel() {
        // 4x3 active grid = 12 verts; capacity 16 leaves 4 inactive/padding slots.
        let resolution_x = 4u32;
        let resolution_y = 3u32;
        const CAPACITY: u32 = 16;
        let size_x = 3.5f32;
        let size_y = 1.25f32;

        // Hand layout: resolution_x(u32), resolution_y(u32), capacity(u32), pad,
        //   size_x(f32), size_y(f32), origin_x(f32), origin_z(f32).
        let mut hand = Vec::new();
        hand.extend_from_slice(&resolution_x.to_le_bytes());
        hand.extend_from_slice(&resolution_y.to_le_bytes());
        hand.extend_from_slice(&CAPACITY.to_le_bytes());
        hand.extend_from_slice(&0u32.to_le_bytes());
        hand.extend_from_slice(&size_x.to_le_bytes());
        hand.extend_from_slice(&size_y.to_le_bytes());
        hand.extend_from_slice(&0.0f32.to_le_bytes());
        hand.extend_from_slice(&0.0f32.to_le_bytes());

        // Generated layout: max_capacity(i32), resolution_x(i32), resolution_y(i32),
        //   size_x(f32), size_y(f32), dispatch_count(u32), 2 pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(2_097_152i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(resolution_x as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(resolution_y as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&size_x.to_le_bytes());
        gen_bytes.extend_from_slice(&size_y.to_le_bytes());
        gen_bytes.extend_from_slice(&CAPACITY.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl = include_str!("shaders/generate_grid_mesh.wgsl");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<GenerateGridMesh>()
            .expect("generate_grid_mesh buffer codegen");

        let from_hand = dispatch_grid(hand_wgsl, CAPACITY, &hand);
        let from_gen = dispatch_grid(&gen_wgsl, CAPACITY, &gen_bytes);

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
            for c in 0..2 {
                assert!(
                    (from_hand[i].uv[c] - from_gen[i].uv[c]).abs() < 1e-6,
                    "vertex {i} uv[{c}]"
                );
            }
        }
    }
}
