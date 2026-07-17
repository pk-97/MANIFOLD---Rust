//! `node.edge_pairs` — synthesise the consecutive-pair edge
//! topology `[(0,1), (1,2), …, (N-2, N-1)]` from a vertex count,
//! optionally closing the loop with `(N-1, 0)`. Pads the tail of the
//! output buffer with `EdgePair::SENTINEL` so downstream `render_lines`
//! filters the inactive slots without drawing garbage.
//!
//! The polyline-topology atom for parametric curve graphs that build
//! their points via `generate_range → array_math → pack_curve_xy`.
//! Closed regular polygons (Triangle, Square, Hexagon, …, 64-gon
//! circle) want `closed = true`; open polylines (signal traces, scan
//! curves) want `closed = false`.
//!
//! Variable-N support: when `count` is wired (port-shadow-param) the
//! active edge count is driven from the upstream curve length. The
//! buffer is sized to `max_capacity`; slots past the active count are
//! `SENTINEL` so a polygon with N = 3 active edges drawn from a 64-slot
//! buffer doesn't smear lines across 61 sentinel-padded vertices.

use std::borrow::Cow;

use crate::generators::mesh_common::EdgePair;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Largest legal vertex count and matching output capacity. 64 covers
/// the Circle variant of ConcentricTunnel (a 64-gon approximation)
/// and every smaller regular polygon; matches `POLYGON_MAX_SIDES` from
/// the legacy `polygon_shape` primitive whose role this atom takes over.
pub const CONSECUTIVE_EDGES_MAX_CAPACITY: u32 = 64;

crate::primitive! {
    name: ConsecutiveEdges,
    type_id: "node.edge_pairs",
    purpose: "Generate consecutive-pair edge topology [(0,1), (1,2), …, (N-2, N-1)] from a vertex count, optionally closed via (N-1, 0). The polyline-topology atom: pair with a points source (generate_range + array_math + pack_curve_xy, or any custom CurvePoint producer) and node.draw_lines to draw a closed regular polygon, an open polyline, or any topology where edges connect consecutive vertex indices. `count` is port-shadow-param so an upstream variable-N source (e.g. a mux driving the active polygon side count) drives the active edge count. Output capacity is `max_capacity`; slots beyond the active count are EdgePair::SENTINEL so downstream draw_lines filters them out — drawing exactly the active topology, never garbage from the inactive tail.",
    inputs: {
        count: ScalarF32 optional,
    },
    outputs: {
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count"),
            label: "Vertex Count",
            ty: ParamType::Int,
            default: ParamValue::Float(4.0),
            range: Some((2.0, CONSECUTIVE_EDGES_MAX_CAPACITY as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("closed"),
            label: "Closed",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Edge Buffer Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(CONSECUTIVE_EDGES_MAX_CAPACITY as f32),
            range: Some((2.0, CONSECUTIVE_EDGES_MAX_CAPACITY as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "`closed = true` emits N edges forming a closed loop ([0,1], [1,2], …, [N-1, 0]); `closed = false` emits N-1 edges as an open strip. Pair with `node.range(end_inclusive=false, active_count=N)` so vertex 0 and vertex N-1 land on geometrically distinct points — the closed (N-1, 0) edge then bridges a real gap rather than collapsing to zero length. Output capacity = `max_capacity` (pre-allocated by chain build); the runtime active count comes from the `count` port-shadow, clamped into `[2, max_capacity]`. Inactive tail = `EdgePair::SENTINEL = (u32::MAX, u32::MAX)` so downstream node.draw_lines's `build_instances_from_edges` filter skips them without drawing a line.",
    examples: [],
    picker: { label: "Edge Pairs", category: Atom },
    summary: "Connects a list of points in order into a single line, pairing each point with the next. Can close the loop back to the start.",
    category: Geometry3D,
    role: Source,
    aliases: ["edge pairs", "consecutive edges", "connect points", "polyline"],
    boundary_reason: NonGpu,
}

impl Primitive for ConsecutiveEdges {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "edges" {
            return None;
        }
        let cap = match params.get("max_capacity") {
            Some(ParamValue::Float(f)) => f.round().max(2.0) as u32,
            _ => CONSECUTIVE_EDGES_MAX_CAPACITY,
        };
        Some(cap.min(CONSECUTIVE_EDGES_MAX_CAPACITY))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadow: wired `count` wins over the param.
        let count_raw = ctx.scalar_or_param("count", 4.0);
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(f)) => f.round().max(2.0) as u32,
            _ => CONSECUTIVE_EDGES_MAX_CAPACITY,
        }
        .min(CONSECUTIVE_EDGES_MAX_CAPACITY);
        let n = (count_raw.round().max(2.0) as u32).min(max_capacity);

        let closed = matches!(ctx.params.get("closed"), Some(ParamValue::Bool(true)) | None);

        let Some(edges_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.edge_pairs: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output.",
            );
            return;
        };
        let edges_cap = (edges_dst.size / std::mem::size_of::<EdgePair>() as u64) as u32;
        if edges_cap == 0 {
            return;
        }

        let mut scratch =
            [EdgePair::SENTINEL; CONSECUTIVE_EDGES_MAX_CAPACITY as usize];
        let n_usize = n as usize;

        // Open-strip span: N-1 consecutive (i, i+1) pairs.
        let open_edges = n_usize.saturating_sub(1);
        for (i, slot) in scratch.iter_mut().take(open_edges).enumerate() {
            *slot = EdgePair {
                a: i as u32,
                b: (i + 1) as u32,
            };
        }
        // Closing edge: (N-1, 0) when closed=true. Only emit if N >= 2
        // (open_edges >= 1) — for N == 1 there is no loop to close.
        if closed && n >= 2 {
            scratch[open_edges] = EdgePair { a: n - 1, b: 0 };
        }

        let write_count = (edges_cap as usize).min(scratch.len());
        // Safety: shared-memory MTLBuffer (chain build pre-allocates),
        // write count clamped to the buffer capacity, sequential
        // executor on the content thread means no GPU race.
        unsafe {
            edges_dst.write(0, bytemuck::cast_slice(&scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

    #[test]
    fn declares_count_input_and_edge_pair_output() {
        assert_eq!(ConsecutiveEdges::TYPE_ID, "node.edge_pairs");
        assert_eq!(ConsecutiveEdges::INPUTS.len(), 1);
        assert_eq!(ConsecutiveEdges::INPUTS[0].name, "count");
        assert!(!ConsecutiveEdges::INPUTS[0].required);
        assert_eq!(
            ConsecutiveEdges::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );

        let edge_layout = ArrayType::of_known::<EdgePair>();
        assert_eq!(ConsecutiveEdges::OUTPUTS.len(), 1);
        assert_eq!(ConsecutiveEdges::OUTPUTS[0].name, "edges");
        assert_eq!(ConsecutiveEdges::OUTPUTS[0].ty, PortType::Array(edge_layout));
    }

    #[test]
    fn declares_count_closed_and_max_capacity_params() {
        let names: Vec<&str> = ConsecutiveEdges::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["count", "closed", "max_capacity"]);
    }

    #[test]
    fn output_capacity_reads_max_capacity_param() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ConsecutiveEdges::new();

        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("max_capacity"), ParamValue::Float(32.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            Some(32),
        );

        // Clamped to the hard upper bound.
        let mut over = ParamValues::default();
        over.insert(std::borrow::Cow::Borrowed("max_capacity"), ParamValue::Float(128.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &over, &[]),
            Some(CONSECUTIVE_EDGES_MAX_CAPACITY),
        );

        // Unknown port returns None.
        let params = ParamValues::default();
        assert!(Primitive::array_output_capacity(&prim, "out", &params, &[]).is_none());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ConsecutiveEdges::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.edge_pairs");
    }
}
