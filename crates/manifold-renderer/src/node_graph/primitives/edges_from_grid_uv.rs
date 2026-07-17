//! `node.grid_edges` — emit the u-wrap + v-wrap wireframe edge
//! topology for an `n × n` parametric grid as `Array<EdgePair>`. Pairs
//! with `node.grid_points` / `node.combine_xyzw` to author any
//! (u, v)-parametric surface in the graph.
//!
//! Each (iu, iv) cell emits two edges: one toward the next u (with
//! modular wrap) and one toward the next v (with modular wrap). Total
//! edge count: `n² × 2`. Vertex indexing matches `generate_grid_uv`'s
//! row-major convention: `idx = iu * n + iv`.
//!
//! Drive `grid_size` from the same scalar source as the paired
//! `generate_grid_uv` so vertex layout and edge indices agree. The
//! atom is the topology counterpart of `node.platonic_solid_edges`: closed
//! mathematical structure, sentinel-padded inactive tail, CPU-write
//! into shared MTLBuffer for downstream CPU consumption by
//! `node.draw_lines`.

use std::borrow::Cow;
use crate::generators::mesh_common::EdgePair;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub use crate::node_graph::primitives::generate_grid_uv::{
    GRID_UV_DEFAULT_SIZE, GRID_UV_MAX_SIZE,
};

crate::primitive! {
    name: EdgesFromGridUv,
    type_id: "node.grid_edges",
    purpose: "Emit the u-wrap + v-wrap wireframe edge topology for an n × n parametric grid as Array<EdgePair>. Pairs with node.grid_points + node.combine_xyzw to author any (u, v)-parametric surface in the graph (Duocylinder, torus, Klein bottle, geodesic sphere, terrain mesh). Each (iu, iv) cell emits two edges: one toward the next u (with modular wrap) and one toward the next v (with modular wrap). Total edge count = n² × 2. Vertex indexing matches generate_grid_uv's row-major convention (idx = iu * n + iv) so the same grid_size scalar should drive both atoms. CPU-write — sentinel-padded inactive tail, same pattern as node.platonic_solid_edges.",
    inputs: {
        grid_size: ScalarF32 optional,
    },
    outputs: {
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("grid_size"),
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Float(GRID_UV_DEFAULT_SIZE as f32),
            range: Some((2.0, GRID_UV_MAX_SIZE as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is grid_size² × 2 at plan time; runtime topology fills [0..n²·2) and sentinel-pads the tail so a smaller-than-buffer active grid (after a grid_size shrink) keeps render_lines's sentinel-skip filter correct. Wraparound makes the topology closed (every grid edge connects two real vertices); for an open mesh (terrain) suppress the wrap edges by clamping rather than wrapping — future work, not currently exposed. Drive the same scalar into generate_grid_uv.grid_size and edges_from_grid_uv.grid_size — port-shadows-param semantics on both — so layout and topology always agree.",
    examples: [],
    picker: { label: "Grid Edges", category: Atom },
    summary: "Outputs the wireframe edges that connect a grid of points, so you can draw the grid as a mesh of lines.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid edges", "edges from grid uv", "wireframe", "topology"],
    boundary_reason: NonGpu,
    extra_fields: {
        scratch: Vec<EdgePair> = Vec::new(),
    },
}

/// Resolve the `grid_size` selector with port-shadows-param. Mirrors
/// `polytope_vertices::read_shape` — `scalar_or_param` matches Float,
/// but the param is stored as Int (ParamValue::Float wrapped). Read
/// both storage variants explicitly so wired and unwired both work.
fn read_grid_size(ctx: &EffectNodeContext<'_, '_>) -> u32 {
    let wired = ctx
        .inputs
        .scalar("grid_size")
        .and_then(|v| v.as_scalar())
        .map(|f| f.round().max(2.0) as u32);
    let raw = wired.unwrap_or_else(|| match ctx.params.get("grid_size") {
        Some(ParamValue::Float(f)) => f.round().max(2.0) as u32,
        _ => GRID_UV_DEFAULT_SIZE,
    });
    raw.min(GRID_UV_MAX_SIZE)
}

fn plan_time_grid_size(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("grid_size") {
        Some(ParamValue::Float(n)) => n.round().max(2.0) as u32,
        _ => GRID_UV_DEFAULT_SIZE,
    }
    .min(GRID_UV_MAX_SIZE)
}

impl Primitive for EdgesFromGridUv {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "edges" {
            return None;
        }
        let n = plan_time_grid_size(params);
        Some(n * n * 2)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let n = read_grid_size(ctx);

        let Some(edge_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.grid_edges: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate this Array<EdgePair>.",
            );
            return;
        };

        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if edge_capacity == 0 {
            return;
        }

        let active_edges = (n * n * 2) as usize;
        self.scratch.clear();
        self.scratch.reserve(active_edges);
        for iu in 0..n {
            for iv in 0..n {
                let idx = iu * n + iv;
                let nu = ((iu + 1) % n) * n + iv;
                self.scratch.push(EdgePair { a: idx, b: nu });
                let nv = iu * n + ((iv + 1) % n);
                self.scratch.push(EdgePair { a: idx, b: nv });
            }
        }

        let write_count = (edge_capacity as usize).min(active_edges);
        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity; sequential
        // executor means no concurrent writer.
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&self.scratch[..write_count]));
        }

        // Sentinel-pad the tail when the pre-allocated buffer is larger
        // than the active edge count (happens after a grid_size shrink).
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_grid_size_input_and_edge_pair_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(EdgesFromGridUv::TYPE_ID, "node.grid_edges");
        assert_eq!(EdgesFromGridUv::INPUTS.len(), 1);
        assert_eq!(EdgesFromGridUv::INPUTS[0].name, "grid_size");
        assert!(!EdgesFromGridUv::INPUTS[0].required);
        assert_eq!(
            EdgesFromGridUv::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32),
        );

        let edge_layout = ArrayType::of_known::<EdgePair>();
        assert_eq!(EdgesFromGridUv::OUTPUTS.len(), 1);
        assert_eq!(EdgesFromGridUv::OUTPUTS[0].name, "edges");
        assert_eq!(
            EdgesFromGridUv::OUTPUTS[0].ty,
            PortType::Array(edge_layout),
        );
    }

    #[test]
    fn output_capacity_is_two_n_squared() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = EdgesFromGridUv::new();

        let default = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &default, &[]),
            Some(GRID_UV_DEFAULT_SIZE * GRID_UV_DEFAULT_SIZE * 2),
        );

        let mut custom = ParamValues::default();
        custom.insert(std::borrow::Cow::Borrowed("grid_size"), ParamValue::Float(16.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &custom, &[]),
            Some(16 * 16 * 2),
        );

        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &default, &[]),
            None,
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = EdgesFromGridUv::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.grid_edges");
    }
}
