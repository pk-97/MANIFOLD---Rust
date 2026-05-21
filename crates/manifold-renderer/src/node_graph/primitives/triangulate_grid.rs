//! `node.triangulate_grid` — convert a positions-only NxM
//! `Array<MeshVertex>` grid into a triangle-list (N-1)×(M-1)×6
//! vertex stream with per-vertex normals computed from
//! finite-difference tangents.
//!
//! Adapter primitive that lets `node.generate_grid_mesh`'s
//! positions feed cleanly into `node.render_3d_mesh` (which expects
//! triangle-list topology). The source grid is read in row-major
//! order (`row * cols + col`); the output is laid out as six
//! consecutive vertices per quad in the canonical
//! 0/1/2-0/2/3-shape used by the legacy MetallicGlass renderer.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TriangulateUniforms {
    src_cols: u32,
    src_rows: u32,
    dst_capacity: u32,
    _pad0: u32,
}

crate::primitive! {
    name: TriangulateGrid,
    type_id: "node.triangulate_grid",
    purpose: "Convert a positions-only NxM Array<MeshVertex> grid into a triangle-list (N-1)*(M-1)*6 vertex stream with finite-difference normals. The adapter primitive between node.generate_grid_mesh (positions) and node.render_3d_mesh (triangle list). For MetallicGlass-shaped graphs: GenerateGridMesh → DisplaceMesh → TriangulateGrid → Render3DMesh.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "src_cols",
            label: "Source Columns",
            ty: ParamType::Int,
            default: ParamValue::Int(256),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "src_rows",
            label: "Source Rows",
            ty: ParamType::Int,
            default: ParamValue::Int(256),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "src_cols × src_rows must match the upstream producer's grid resolution. Output capacity must be at least (src_cols - 1) × (src_rows - 1) × 6 vertices. Default 256×256 grid → 390,150 triangle vertices ≈ 12.5 MB. Border normals are clamped to the nearest in-bounds neighbour (no special-case ghost rows). Source must be in row-major order: idx = row * cols + col.",
    examples: [],
    picker: { label: "Triangulate Grid", category: Atom },
}

impl Primitive for TriangulateGrid {
    /// Output capacity = `(src_cols-1) × (src_rows-1) × 6` triangle
    /// vertices — the canonical 6-vertex-per-quad triangulation.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let read_dim = |name| match params.get(name) {
            Some(ParamValue::Int(n)) => Some((*n).max(2) as u32),
            _ => None,
        };
        let cols = read_dim("src_cols")?;
        let rows = read_dim("src_rows")?;
        Some((cols - 1).saturating_mul(rows - 1).saturating_mul(6))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let src_cols = match ctx.params.get("src_cols") {
            Some(ParamValue::Int(n)) => (*n).max(2) as u32,
            _ => 256,
        };
        let src_rows = match ctx.params.get("src_rows") {
            Some(ParamValue::Int(n)) => (*n).max(2) as u32,
            _ => 256,
        };

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let dst_capacity = (dst.size / vertex_size) as u32;
        if dst_capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/triangulate_grid.wgsl"),
                "cs_main",
                "node.triangulate_grid",
            )
        });

        let uniforms = TriangulateUniforms {
            src_cols,
            src_rows,
            dst_capacity,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [dst_capacity.div_ceil(64), 1, 1],
            "node.triangulate_grid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn triangulate_grid_declares_mesh_array_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<MeshVertex>() as u32,
            item_align: std::mem::align_of::<MeshVertex>() as u32,
        };
        assert_eq!(TriangulateGrid::TYPE_ID, "node.triangulate_grid");
        assert_eq!(TriangulateGrid::INPUTS.len(), 1);
        assert_eq!(TriangulateGrid::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(TriangulateGrid::OUTPUTS.len(), 1);
        assert_eq!(TriangulateGrid::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn triangulate_grid_has_cols_and_rows_params() {
        let names: Vec<&str> = TriangulateGrid::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["src_cols", "src_rows"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TriangulateGrid::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.triangulate_grid");
    }
}
