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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    resolution_x: u32,
    resolution_y: u32,
    capacity: u32,
    _pad0: u32,
    size_x: f32,
    size_y: f32,
    origin_x: f32,
    origin_z: f32,
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
    composition_notes: "max_capacity ≥ resolution_x × resolution_y. The chain build pre-allocates max_capacity × 32 bytes and triggers a rebuild when changed; resolution sliders only write uniforms. Default 256×256 = 65k vertices ≈ 2 MB. size_x / size_y are port-shadows-param: aspect-correct the mesh by wiring `system.generator_input.aspect → math.multiply(b=2.0) → size_x` (matches the legacy MetallicGlass mesh that spans [-aspect, +aspect] in X).",
    examples: [],
    picker: { label: "Grid Mesh", category: Atom },
    summary: "Builds a flat grid of points as a 3D mesh, the base for terrain, cloth, and displacement looks. Pair it with Surface Bumps or Push Mesh.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid mesh", "generate grid mesh", "plane", "terrain", "Grid SOP"],
    boundary_reason: ConversionDebt,
}

impl Primitive for GenerateGridMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
        let active = (resolution_x as u64 * resolution_y as u64).min(capacity as u64) as u32;
        let _ = active;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_grid_mesh.wgsl"),
                "cs_main",
                "node.grid_mesh",
            )
        });

        let uniforms = GridUniforms {
            resolution_x,
            resolution_y,
            capacity,
            _pad0: 0,
            size_x,
            size_y,
            origin_x: 0.0,
            origin_z: 0.0,
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
            [capacity.div_ceil(64), 1, 1],
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
