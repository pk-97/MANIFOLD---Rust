//! `node.generate_duocylinder_vertices` — emit a parametric 4D torus
//! grid (duocylinder surface) scaled to magnitude 0.25 plus its u/v
//! neighbor wireframe topology as paired `Array<Vec4Vertex>` +
//! `Array<EdgePair>` outputs.
//!
//! Vertex positions: `(cos u, sin u, cos v, sin v) * (0.25 / sqrt(2))`
//! for (u, v) ∈ [0, 2π)² with `grid_size` steps each. Every duocylinder
//! point has 4D magnitude `sqrt(2)` pre-scaling, so the factor
//! `0.25 / sqrt(2)` ≈ 0.1768 normalises to a 0.25 sphere — matching
//! the legacy `generator_math::PROJ_SCALE` screen-fit factor. Index
//! ordering is row-major in (u, v): `idx = iu * grid_size + iv`.
//!
//! Edges: each (iu, iv) emits two edges — one toward the next u (with
//! wrap) and one toward the next v (with wrap). Total = grid_size² × 2.
//! Edges live in a CPU-written shared MTLBuffer alongside vertices to
//! match the [`crate::node_graph::primitives::WireframeShape`] pattern.
//!
//! The 0.25 magnitude lives inside this primitive (not as a graph-side
//! math node) so downstream `project_4d.proj_scale` defaults to 1.0 —
//! same pattern as `wireframe_shape` and `generate_tesseract_vertices`.
//! 4D perspective is non-linear in w, so this bake does not reproduce
//! the legacy generator's projected pixels bit-exactly.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{EdgePair, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const DUOCYLINDER_DEFAULT_GRID_SIZE: u32 = 24;
pub const DUOCYLINDER_MAX_GRID_SIZE: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    grid_size: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GenerateDuocylinderVertices,
    type_id: "node.generate_duocylinder_vertices",
    purpose: "Emit a parametric 4D torus (duocylinder surface) grid scaled to magnitude 0.25 plus its u/v neighbor wireframe topology as paired Array<Vec4Vertex> + Array<EdgePair>. Vertex positions are (cos u, sin u, cos v, sin v) * (0.25 / sqrt(2)) for (u, v) sampled at grid_size steps each across [0, 2π). Feed both outputs through node.rotate_4d → node.project_4d → node.render_lines (with the `edges` input wired) to reproduce the legacy Duocylinder generator's pipeline. The 4D parametric-surface counterpart of node.wireframe_shape.",
    inputs: {},
    outputs: {
        vertices: Array(Vec4Vertex),
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: "grid_size",
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Int(24),
            range: Some((4.0, DUOCYLINDER_MAX_GRID_SIZE as f32),),
            enum_values: &[],
        },
    ],
    composition_notes: "Output vertex count is grid_size² (default 24² = 576); edge count is grid_size² × 2 (default 1152). Index order is row-major: idx = iu * grid_size + iv. Each vertex emits one edge toward (iu+1, iv) and one toward (iu, iv+1) with wraparound — the standard 4D torus wireframe. Vertices are pre-scaled to 0.25 magnitude (matching PROJ_SCALE) so downstream project_4d.proj_scale defaults to 1.0. Buffer capacities are sized from grid_size at plan time; changing grid_size at runtime triggers a chain rebuild.",
    examples: [],
    picker: { label: "Generate Duocylinder Vertices", category: Atom },
    extra_fields: {
        edges_scratch: Vec<EdgePair> = Vec::new(),
    },
}

/// Read `grid_size` from the params bag, clamped to the primitive's
/// valid range. Shared by `array_output_capacity` (plan time) and
/// `run` (frame time) so the buffer sizes match the dispatch.
fn read_grid_size(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("grid_size") {
        Some(ParamValue::Int(n)) => (*n).max(2) as u32,
        _ => DUOCYLINDER_DEFAULT_GRID_SIZE,
    }
    .min(DUOCYLINDER_MAX_GRID_SIZE)
}

impl Primitive for GenerateDuocylinderVertices {
    /// Output sizing tracks `grid_size`:
    /// - `vertices`: grid_size²
    /// - `edges`:    grid_size² × 2 (one u-neighbor + one v-neighbor per vertex)
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let n = read_grid_size(params);
        let verts = n * n;
        match port_name {
            "vertices" => Some(verts),
            "edges" => Some(verts * 2),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid_size = read_grid_size(ctx.params);

        let Some(vert_dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let Some(edge_dst) = ctx.outputs.array("edges") else {
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let vert_capacity = (vert_dst.size / vertex_size) as u32;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if vert_capacity == 0 || edge_capacity == 0 {
            return;
        }

        // ── CPU-write edges ──
        // Topology depends only on grid_size, so recompute each frame
        // (cheap) and write into the shared MTLBuffer. Sentinel-pad if
        // the pre-allocated buffer is larger than the active count
        // (which happens when grid_size shrinks at runtime — the buffer
        // was sized at compile time from the old grid_size).
        let active_edges = (grid_size * grid_size * 2) as usize;
        self.edges_scratch.clear();
        self.edges_scratch.reserve(active_edges);
        for iu in 0..grid_size {
            for iv in 0..grid_size {
                let idx = iu * grid_size + iv;
                let nu = ((iu + 1) % grid_size) * grid_size + iv;
                self.edges_scratch.push(EdgePair { a: idx, b: nu });
                let nv = iu * grid_size + ((iv + 1) % grid_size);
                self.edges_scratch.push(EdgePair { a: idx, b: nv });
            }
        }
        let write_count = (edge_capacity as usize).min(active_edges);
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&self.edges_scratch[..write_count]));
        }
        // Sentinel-pad the tail when the buffer is larger than active.
        // This matches the wireframe_shape convention; render_lines
        // skips sentinel pairs when walking the edges buffer.
        if write_count < edge_capacity as usize {
            const PAD_CHUNK: usize = 64;
            const TAIL: [EdgePair; PAD_CHUNK] = [EdgePair::SENTINEL; PAD_CHUNK];
            let mut offset = write_count;
            while offset < edge_capacity as usize {
                let chunk = (edge_capacity as usize - offset).min(PAD_CHUNK);
                unsafe {
                    edge_dst.write(
                        (offset as u64) * edge_size,
                        bytemuck::cast_slice(&TAIL[..chunk]),
                    );
                }
                offset += chunk;
            }
        }

        // ── Dispatch the vertex-write compute shader ──
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_duocylinder_vertices.wgsl"),
                "cs_main",
                "node.generate_duocylinder_vertices",
            )
        });

        let uniforms = Uniforms {
            grid_size,
            capacity: vert_capacity,
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
                    buffer: vert_dst,
                    offset: 0,
                },
            ],
            [grid_size.div_ceil(8), grid_size.div_ceil(8), 1],
            "node.generate_duocylinder_vertices",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_vec4_plus_edge_array_outputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec4_layout = ArrayType {
            item_size: std::mem::size_of::<Vec4Vertex>() as u32,
            item_align: std::mem::align_of::<Vec4Vertex>() as u32,
        };
        let edge_layout = ArrayType {
            item_size: std::mem::size_of::<EdgePair>() as u32,
            item_align: std::mem::align_of::<EdgePair>() as u32,
        };
        assert_eq!(
            GenerateDuocylinderVertices::TYPE_ID,
            "node.generate_duocylinder_vertices"
        );
        assert!(GenerateDuocylinderVertices::INPUTS.is_empty());
        assert_eq!(GenerateDuocylinderVertices::OUTPUTS.len(), 2);
        assert_eq!(GenerateDuocylinderVertices::OUTPUTS[0].name, "vertices");
        assert_eq!(
            GenerateDuocylinderVertices::OUTPUTS[0].ty,
            PortType::Array(vec4_layout)
        );
        assert_eq!(GenerateDuocylinderVertices::OUTPUTS[1].name, "edges");
        assert_eq!(
            GenerateDuocylinderVertices::OUTPUTS[1].ty,
            PortType::Array(edge_layout)
        );
    }

    #[test]
    fn capacities_scale_with_grid_size() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = GenerateDuocylinderVertices::new();

        // Default grid_size = 24 → 576 verts, 1152 edges
        let default_params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &default_params, &[]),
            Some(24 * 24)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &default_params, &[]),
            Some(24 * 24 * 2)
        );

        // Custom grid_size = 16 → 256 verts, 512 edges
        let mut custom = ParamValues::default();
        custom.insert("grid_size", ParamValue::Int(16));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &custom, &[]),
            Some(16 * 16)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &custom, &[]),
            Some(16 * 16 * 2)
        );

        // Clamped to DUOCYLINDER_MAX_GRID_SIZE
        let mut huge = ParamValues::default();
        huge.insert("grid_size", ParamValue::Int(128));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &huge, &[]),
            Some(DUOCYLINDER_MAX_GRID_SIZE * DUOCYLINDER_MAX_GRID_SIZE)
        );

        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &default_params, &[]),
            None
        );
    }

    #[test]
    fn registers_with_palette() {
        let prim = GenerateDuocylinderVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.generate_duocylinder_vertices"
        );
    }
}
