//! `node.generate_platonic_solid` — emit one of five Platonic
//! solid vertex sets as an `Array<MeshVertex>`.
//!
//! Vertex positions ported from `generators/wireframe_zoo.rs`'s
//! Rust-side const tables, normalised to the unit sphere in-shader.
//! Output normals point radially outward (matches the natural normal
//! for a vertex on a convex polyhedron).
//!
//! Vertex counts: Tetrahedron = 4, Cube = 8, Octahedron = 6,
//! Icosahedron = 12, Dodecahedron = 20.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const PLATONIC_SHAPES: &[&str] = &[
    "Tetrahedron",
    "Cube",
    "Octahedron",
    "Icosahedron",
    "Dodecahedron",
];

/// Maximum vertex count across all shapes. Use this when sizing the
/// downstream buffer; the primitive zero-pads unused slots.
pub const PLATONIC_MAX_VERTS: u32 = 20;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlatonicUniforms {
    shape: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GeneratePlatonicSolid,
    type_id: "node.generate_platonic_solid",
    purpose: "Emit one of five Platonic solid vertex sets (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as an Array<MeshVertex>. Positions normalised to the unit sphere; normals point radially outward. The vertex-set building block for WireframeZoo-shaped graphs — feed downstream into node.rotate_3d, node.project_3d, then a line-segment renderer (edge connectivity is downstream concern).",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(20),
            range: Some((4.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "shape",
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: PLATONIC_SHAPES,
        },
    ],
    composition_notes: "Output is the UNIQUE vertex set (not pair-expanded for edge rendering). For wireframe rendering through node.render_lines, downstream needs an adapter that pair-expands the edges per shape — the edge connectivity tables live in wireframe_zoo.rs. max_capacity = 20 fits the largest shape (Dodecahedron); smaller buffers truncate at runtime. Vertex counts: Tetra=4, Cube=8, Octa=6, Icosa=12, Dodeca=20.",
    examples: [],
    picker: { label: "Generate Platonic Solid", category: Atom },
}

impl Primitive for GeneratePlatonicSolid {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let shape = match ctx.params.get("shape") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
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
                include_str!("shaders/generate_platonic_solid.wgsl"),
                "cs_main",
                "node.generate_platonic_solid",
            )
        });

        let uniforms = PlatonicUniforms {
            shape,
            capacity,
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
            "node.generate_platonic_solid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_platonic_declares_zero_inputs_and_mesh_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<MeshVertex>() as u32,
            item_align: std::mem::align_of::<MeshVertex>() as u32,
        };
        assert_eq!(GeneratePlatonicSolid::TYPE_ID, "node.generate_platonic_solid");
        assert!(GeneratePlatonicSolid::INPUTS.is_empty());
        assert_eq!(GeneratePlatonicSolid::OUTPUTS.len(), 1);
        assert_eq!(
            GeneratePlatonicSolid::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn generate_platonic_has_five_shape_options() {
        let shape = GeneratePlatonicSolid::PARAMS
            .iter()
            .find(|p| p.name == "shape")
            .unwrap();
        assert_eq!(shape.ty, ParamType::Enum);
        assert_eq!(shape.enum_values.len(), 5);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GeneratePlatonicSolid::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_platonic_solid");
    }
}
