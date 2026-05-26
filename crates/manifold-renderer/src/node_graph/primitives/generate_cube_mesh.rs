//! `node.generate_cube_mesh` — emit a unit cube as 36 triangle-list
//! `MeshVertex` entries (6 faces × 2 triangles × 3 vertices) with
//! per-face outward normals.
//!
//! Vertex data ported from
//! `generators/shaders/digital_plants_render.wgsl`'s hardcoded
//! cube constants. Pair with `node.render_instanced_3d_mesh` to
//! draw N copies of a cube under different transforms — the
//! decomposed shape of NestedCubes / DigitalPlants.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Number of triangle vertices in a cube mesh (6 faces × 2 triangles × 3 vertices).
/// Use this when sizing buffers for downstream consumers.
pub const CUBE_VERTEX_COUNT: u32 = 36;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CubeUniforms {
    capacity: u32,
    size: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GenerateCubeMesh,
    type_id: "node.generate_cube_mesh",
    purpose: "Emit a unit cube as 36 triangle-list MeshVertex entries (6 faces × 2 triangles × 3 vertices) with per-face outward normals. The cube-shape building block for NestedCubes / DigitalPlants and any instanced-cube graph: pair with node.generate_instance_transforms + node.render_instanced_3d_mesh to draw a field of cubes.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(36.0),
            range: Some((36.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "size",
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity is the chain-build pre-allocation ceiling — defaults to 36 (exactly one cube). Larger values pad the buffer with zero-vertex entries; useful only if downstream consumers expect a multi-mesh buffer. size scales the [-0.5, 0.5] unit cube. For non-cube wireframe shapes use node.polytope_vertices + node.polytope_edges.",
    examples: [],
    picker: { label: "Generate Cube Mesh", category: Atom },
}

impl Primitive for GenerateCubeMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let size = match ctx.params.get("size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_cube_mesh.wgsl"),
                "cs_main",
                "node.generate_cube_mesh",
            )
        });

        let uniforms = CubeUniforms {
            capacity,
            size,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.generate_cube_mesh",
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
        assert_eq!(GenerateCubeMesh::TYPE_ID, "node.generate_cube_mesh");
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
        assert_eq!(node.type_id().as_str(), "node.generate_cube_mesh");
    }
}
