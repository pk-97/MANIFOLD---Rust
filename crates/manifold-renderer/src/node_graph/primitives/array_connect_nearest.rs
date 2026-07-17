//! `node.connect_nearest` — for each item in a
//! `Channels[X, Y, WIDTH, HEIGHT]` array, find its nearest neighbour
//! within a distance threshold and emit the pair as an edge.
//!
//! Output: `Channels[A_INDEX: U32, B_INDEX: U32]` (EdgePair-compatible).
//! One edge per item that has a qualifying neighbour; items without a
//! neighbour within `max_distance` produce no edge. Edges are
//! undirected — (i, j) means "item i's nearest is j."
//!
//! First consumer: Blob Track (connection lines between nearby blobs).
//! Reusable for particle proximity graphs, constellation effects,
//! sparse-detection neighbour viz.

use crate::generators::mesh_common::EdgePair;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

crate::primitive! {
    name: ArrayConnectNearest,
    type_id: "node.connect_nearest",
    purpose: "For each item in a Channels[X, Y, WIDTH, HEIGHT] array, find its nearest neighbour within max_distance and emit an EdgePair (A_INDEX, B_INDEX). Sparse nearest-neighbour graph generation. Wire detection regions, particle positions, or any sparse-position array; output connects to render_lines edges port for connection-line visualisation.",
    inputs: {
        in: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        max_distance: ScalarF32 optional,
    },
    outputs: {
        edges: Channels[A_INDEX: U32, B_INDEX: U32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_distance"),
            label: "Max Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(0.59),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_edges"),
            label: "Max Edges",
            ty: ParamType::Int,
            default: ParamValue::Float(32.0),
            range: Some((1.0, 256.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blob_detect_ffi or track_persist output → this primitive's `in` port. Output `edges` connects to render_lines' `edges` port; the same `in` source wires to render_lines' `points` port (via pack_curve_xy or a wgsl_compute that extracts X,Y). max_distance is in the same coordinate space as the input (normalised 0..1 for blob detections). Each item gets at most one edge to its nearest qualifying neighbour.",
    examples: [],
    picker: { label: "Connect Nearest", category: Driver },
    summary: "For each item in a list, finds its nearest neighbour and emits a connecting line. Used to draw constellations between tracked blobs.",
    category: MathAndConvert,
    role: Control,
    aliases: ["connect nearest", "array connect nearest", "nearest neighbour", "constellation"],
    boundary_reason: NonGpu,
    // PARAM_RANGE_CONTRACT_DESIGN.md D6/§2 mechanical grant: `max_edges`
    // sizes the output array — `array_output_capacity` (this file) returns
    // `Some(max_edges)` verbatim as the allocated `edges` buffer capacity.
    // `max_distance` was VERIFIED (P2 §2 read) and rejected: `run()` (this
    // file) only ever squares it into a comparison threshold — no division,
    // no degenerate collapse at 0, so it stays a display hint.
    param_contracts: [
        ("max_edges", manifold_core::effects::RangeContract {
            min: Some(1.0),
            max: None,
            reason: manifold_core::effects::RangeReason::Count,
        }),
    ],
    extra_fields: {
        last_edge_count: Option<usize> = None,
    },
}

impl Primitive for ArrayConnectNearest {
    // Data-driven skip, reporter side: a frame that emitted zero edges
    // (fewer than two live detections, or none within range) reports
    // empty so downstream `empty_skip_input_ports` declarers (the Draw
    // connection atoms) can skip.
    fn reports_empty_output(&self) -> bool {
        self.last_edge_count == Some(0)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "edges" {
            let max_edges = match params.get("max_edges") {
                Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
                _ => 32,
            };
            Some(max_edges)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.last_edge_count = None;
        let max_distance = ctx.scalar_or_param("max_distance", 0.59);
        let threshold_sq = max_distance * max_distance;
        let max_edges = match ctx.params.get("max_edges") {
            Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
            _ => 32,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("edges") else {
            return;
        };

        let item_size = 16u64; // 4 × f32
        let in_capacity = (in_buf.size / item_size) as usize;

        let in_ptr = in_buf
            .mapped_ptr()
            .expect("array_connect_nearest: input must be shared-memory");
        let in_floats: &[f32] =
            unsafe { std::slice::from_raw_parts(in_ptr as *const f32, in_capacity * 4) };

        // Count active items (non-zero width+height).
        let mut item_count = 0usize;
        for i in 0..in_capacity {
            let w = in_floats[i * 4 + 2];
            let h = in_floats[i * 4 + 3];
            if w > 0.0001 || h > 0.0001 {
                item_count = i + 1;
            }
        }

        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let out_capacity = (out_buf.size / edge_size) as usize;
        let edge_limit = (max_edges as usize).min(out_capacity);

        let out_ptr = out_buf
            .mapped_ptr()
            .expect("array_connect_nearest: output must be shared-memory");
        let out_edges: &mut [EdgePair] = unsafe {
            std::slice::from_raw_parts_mut(out_ptr as *mut EdgePair, out_capacity)
        };

        // For each item, find its nearest unvisited neighbour within
        // threshold. Same algorithm as legacy compute_connections.
        let mut edge_count = 0usize;
        for i in 0..item_count {
            if edge_count >= edge_limit {
                break;
            }
            let ax = in_floats[i * 4];
            let ay = in_floats[i * 4 + 1];
            let aw = in_floats[i * 4 + 2];
            let ah = in_floats[i * 4 + 3];
            if aw <= 0.0001 && ah <= 0.0001 {
                continue;
            }

            let mut best_dist = f32::MAX;
            let mut best_j: i32 = -1;

            for j in (i + 1)..item_count {
                let bw = in_floats[j * 4 + 2];
                let bh = in_floats[j * 4 + 3];
                if bw <= 0.0001 && bh <= 0.0001 {
                    continue;
                }
                let bx = in_floats[j * 4];
                let by = in_floats[j * 4 + 1];
                let dx = ax - bx;
                let dy = ay - by;
                let dist = dx * dx + dy * dy;
                if dist < best_dist && dist < threshold_sq {
                    best_dist = dist;
                    best_j = j as i32;
                }
            }

            if best_j >= 0 {
                out_edges[edge_count] = EdgePair {
                    a: i as u32,
                    b: best_j as u32,
                };
                edge_count += 1;
            }
        }

        // Sentinel-fill remaining slots.
        for slot in &mut out_edges[edge_count..out_capacity] {
            *slot = EdgePair::SENTINEL;
        }
        self.last_edge_count = Some(edge_count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn array_connect_nearest_declares_channels_io() {
        use crate::node_graph::ports::PortType;
        assert_eq!(ArrayConnectNearest::TYPE_ID, "node.connect_nearest");
        assert_eq!(ArrayConnectNearest::INPUTS.len(), 2);
        assert_eq!(ArrayConnectNearest::INPUTS[0].name, "in");
        assert!(matches!(ArrayConnectNearest::INPUTS[0].ty, PortType::Array(_)));
        assert_eq!(ArrayConnectNearest::OUTPUTS.len(), 1);
        assert_eq!(ArrayConnectNearest::OUTPUTS[0].name, "edges");
        let names: Vec<&str> = ArrayConnectNearest::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["max_distance", "max_edges"]);
    }

    #[test]
    fn array_connect_nearest_registers() {
        let prim = ArrayConnectNearest::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.connect_nearest");
    }
}
