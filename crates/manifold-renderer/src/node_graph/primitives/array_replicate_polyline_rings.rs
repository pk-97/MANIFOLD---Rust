//! `node.repeat_outline` — stack K transformed copies
//! of a polyline (outline + edges) into one concatenated polyline,
//! with per-ring uniform scale and per-ring index-shifted edges.
//!
//! The polyline-replicator atom: takes a single polyline (an
//! `Array<CurvePoint>` outline + parallel `Array<EdgePair>` edge
//! topology — typically a polygon ring from
//! `generate_range → array_math → pack_curve_xy` + `consecutive_edges`)
//! and produces K concatenated rings ready for `node.draw_lines`.
//!
//! Per ring i (for i in `[0, ring_count)`):
//!   - `outline_out[i*M..(i+1)*M] = outline[..M] * scales[i]`
//!   - `edges_out[i*M..(i+1)*M] = edges[..M]` index-shifted by `i*M`,
//!     `EdgePair::SENTINEL` preserved as `SENTINEL` (the inactive tail
//!     of every input ring stays inactive in every output ring).
//!
//! `ring_count` at runtime = `min(scales_capacity, max_rings_param)`.
//! Output capacities are `input_outline_capacity * max_rings` and
//! `input_edges_capacity * max_rings` so the chain build pre-allocates
//! the full stack regardless of the runtime ring count.

use std::borrow::Cow;

use crate::generators::mesh_common::{CurvePoint, EdgePair};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Largest legal ring count. Sets the output capacity multiplier at
/// plan time; 32 matches the legacy `concentric_outlines` cap and
/// covers every realistic concentric depth (a 32-ring stack at
/// ring_spacing ≥ 0.05 already extends past the viewport corner).
pub const REPLICATE_MAX_RINGS: u32 = 32;

/// Largest per-ring outline capacity supported by the stack-allocated
/// scratch path. Sized to handle the 64-vertex polygon outline used by
/// ConcentricTunnel with margin.
const REPLICATE_INLINE_SCRATCH: usize = 128;

crate::primitive! {
    name: ArrayReplicatePolylineRings,
    type_id: "node.repeat_outline",
    purpose: "Stack K transformed copies of a polyline (outline + edge topology) into one concatenated polyline. Per-ring uniform scale on the outline; per-ring index shift on the edges (sentinel-preserving). The K-fold replication atom for line-based generators: pair a single polygon / Lissajous / Rose curve outline + edges with a `scales` Array<f32> (from generate_range + array_math) to produce concentric, parallax, or stacked variations of the source polyline. The ring count is `min(scales.capacity, max_rings)`; output capacity is `input.capacity * max_rings` so the chain build pre-allocates the full stack at plan time.",
    inputs: {
        outline: Array(CurvePoint) required,
        edges: Array(EdgePair) required,
        scales: Array(f32) required,
    },
    outputs: {
        outline: Array(CurvePoint),
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_rings"),
            label: "Max Rings",
            ty: ParamType::Int,
            default: ParamValue::Float(16.0),
            range: Some((1.0, REPLICATE_MAX_RINGS as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is `input.capacity * max_rings` for both `outline` and `edges`; runtime `ring_count = min(scales.capacity, max_rings)`. Ring i transform: outline points multiplied by the scalar `scales[i]`; edge pairs index-shifted by `i * input.outline.capacity` so each ring references its own vertex slice. `EdgePair::SENTINEL` in the input edges propagates as `SENTINEL` in every output ring (sentinels are slots downstream render_lines skips — the inactive tail of a variable-N polygon stays inactive in every replicated ring). The per-ring stride is the INPUT outline capacity, not the active vertex count — this matches consecutive_edges's index space, where active edges reference indices `[0..N)` and `SENTINEL` fills `[N..capacity)`.",
    examples: [],
    picker: { label: "Repeat Outline (rings)", category: Atom },
    summary: "Stacks scaled copies of an outline into concentric rings, turning one shape into a set of nested rings.",
    category: Geometry3D,
    role: Filter,
    aliases: ["repeat outline", "array replicate polyline rings", "rings", "concentric", "replicate"],
    boundary_reason: NonGpu,
}

impl Primitive for ArrayReplicatePolylineRings {
    /// Output capacity scales with `max_rings` (param) × the upstream
    /// input capacity. `ring_count` at runtime is bounded by both
    /// `max_rings` and the actual `scales` buffer capacity, so the
    /// allocated buffer is always large enough to hold the active stack.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let max_rings = match params.get("max_rings") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as u32,
            _ => 16,
        }
        .min(REPLICATE_MAX_RINGS);

        let src = match port_name {
            "outline" => "outline",
            "edges" => "edges",
            _ => return None,
        };
        input_capacities
            .iter()
            .find(|(p, _)| *p == src)
            .map(|(_, n)| n * max_rings)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_rings = match ctx.params.get("max_rings") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as u32,
            _ => 16,
        }
        .min(REPLICATE_MAX_RINGS);

        // ── Resolve buffers ──
        let Some(outline_in) = ctx.inputs.array("outline") else {
            return;
        };
        let Some(edges_in) = ctx.inputs.array("edges") else {
            return;
        };
        let Some(scales_in) = ctx.inputs.array("scales") else {
            return;
        };
        let Some(outline_out) = ctx.outputs.array("outline") else {
            log::warn!(
                "node.repeat_outline: no GpuBuffer bound to output port \
                 `outline` — the chain build did not pre-allocate the Array<CurvePoint> output."
            );
            return;
        };
        let Some(edges_out) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.repeat_outline: no GpuBuffer bound to output port \
                 `edges` — the chain build did not pre-allocate the Array<EdgePair> output."
            );
            return;
        };

        let pt_size = std::mem::size_of::<CurvePoint>() as u64;
        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let f32_size = std::mem::size_of::<f32>() as u64;

        let in_outline_cap = (outline_in.size / pt_size) as u32;
        let in_edges_cap = (edges_in.size / edge_size) as u32;
        let scales_cap = (scales_in.size / f32_size) as u32;
        let out_outline_cap = (outline_out.size / pt_size) as u32;
        let out_edges_cap = (edges_out.size / edge_size) as u32;
        if in_outline_cap == 0
            || in_edges_cap == 0
            || scales_cap == 0
            || out_outline_cap == 0
            || out_edges_cap == 0
        {
            return;
        }

        // Runtime ring count: bounded by both the user-set max_rings and
        // the actual scales buffer capacity (which is the upstream
        // generator's allocation — usually generate_range's max_capacity).
        let ring_count = max_rings.min(scales_cap);

        // ── Read inputs via mapped_ptr ──
        // Shared MTLBuffers expose a persistent CPU-visible pointer;
        // sequential executor on the content thread means upstream writes
        // are visible by the time this primitive runs.
        let outline_in_ptr = outline_in
            .mapped_ptr()
            .expect("array_replicate_polyline_rings: outline input must be shared-memory");
        let edges_in_ptr = edges_in
            .mapped_ptr()
            .expect("array_replicate_polyline_rings: edges input must be shared-memory");
        let scales_ptr = scales_in
            .mapped_ptr()
            .expect("array_replicate_polyline_rings: scales input must be shared-memory");

        let in_outline: &[CurvePoint] = unsafe {
            std::slice::from_raw_parts(
                outline_in_ptr as *const CurvePoint,
                in_outline_cap as usize,
            )
        };
        let in_edges: &[EdgePair] = unsafe {
            std::slice::from_raw_parts(edges_in_ptr as *const EdgePair, in_edges_cap as usize)
        };
        let scales: &[f32] = unsafe {
            std::slice::from_raw_parts(scales_ptr as *const f32, scales_cap as usize)
        };

        let outline_items = (ring_count as usize) * (in_outline_cap as usize);
        let edges_items = (ring_count as usize) * (in_edges_cap as usize);
        let outline_write_count = (out_outline_cap as usize).min(outline_items);
        let edges_write_count = (out_edges_cap as usize).min(edges_items);

        // Per-ring scratch sized to one input ring's worth. Stack
        // allocation avoids per-call Vec allocation; the inline scratch
        // size is checked against the input capacity below.
        assert!(
            (in_outline_cap as usize) <= REPLICATE_INLINE_SCRATCH,
            "array_replicate_polyline_rings: input outline capacity {} exceeds inline scratch \
             size {}; bump REPLICATE_INLINE_SCRATCH if a producer is wider",
            in_outline_cap,
            REPLICATE_INLINE_SCRATCH,
        );
        assert!(
            (in_edges_cap as usize) <= REPLICATE_INLINE_SCRATCH,
            "array_replicate_polyline_rings: input edges capacity {} exceeds inline scratch \
             size {}",
            in_edges_cap,
            REPLICATE_INLINE_SCRATCH,
        );

        for i in 0..ring_count {
            let ring_outline_offset = (i as usize) * (in_outline_cap as usize);
            let ring_edges_offset = (i as usize) * (in_edges_cap as usize);
            if ring_outline_offset >= outline_write_count {
                break;
            }
            let scale = scales[i as usize];

            // Outline: per-vertex uniform scale into stack scratch.
            let outline_chunk =
                (outline_write_count - ring_outline_offset).min(in_outline_cap as usize);
            let mut outline_scratch =
                [CurvePoint { xy: [0.0, 0.0] }; REPLICATE_INLINE_SCRATCH];
            for j in 0..outline_chunk {
                let p = in_outline[j].xy;
                outline_scratch[j] = CurvePoint {
                    xy: [p[0] * scale, p[1] * scale],
                };
            }

            // Edges: shift indices by ring offset, propagate sentinels.
            // Stride is the INPUT outline capacity (matches the
            // outline_out indexing — each ring owns vertex slice
            // [i*in_outline_cap, (i+1)*in_outline_cap)).
            let edges_chunk = if ring_edges_offset >= edges_write_count {
                0
            } else {
                (edges_write_count - ring_edges_offset).min(in_edges_cap as usize)
            };
            let mut edges_scratch = [EdgePair::SENTINEL; REPLICATE_INLINE_SCRATCH];
            for j in 0..edges_chunk {
                let e = in_edges[j];
                edges_scratch[j] = if e.a == u32::MAX || e.b == u32::MAX {
                    EdgePair::SENTINEL
                } else {
                    EdgePair {
                        a: e.a + i * in_outline_cap,
                        b: e.b + i * in_outline_cap,
                    }
                };
            }

            // Safety: shared-memory MTLBuffer pre-bound by the chain
            // build, offsets clamped to the buffer capacities, no GPU
            // pass races these writes (sequential executor on the
            // content thread).
            unsafe {
                outline_out.write(
                    (ring_outline_offset as u64) * pt_size,
                    bytemuck::cast_slice(&outline_scratch[..outline_chunk]),
                );
                if edges_chunk > 0 {
                    edges_out.write(
                        (ring_edges_offset as u64) * edge_size,
                        bytemuck::cast_slice(&edges_scratch[..edges_chunk]),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{ArrayType, PortType};

    #[test]
    fn declares_three_array_inputs_and_two_array_outputs() {
        assert_eq!(
            ArrayReplicatePolylineRings::TYPE_ID,
            "node.repeat_outline"
        );

        let f32_layout = ArrayType::of_known::<f32>();
        let pt_layout = ArrayType::of_known::<CurvePoint>();
        let edge_layout = ArrayType::of_known::<EdgePair>();

        let ins = ArrayReplicatePolylineRings::INPUTS;
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "outline");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Array(pt_layout));
        assert_eq!(ins[1].name, "edges");
        assert!(ins[1].required);
        assert_eq!(ins[1].ty, PortType::Array(edge_layout));
        assert_eq!(ins[2].name, "scales");
        assert!(ins[2].required);
        assert_eq!(ins[2].ty, PortType::Array(f32_layout));

        let outs = ArrayReplicatePolylineRings::OUTPUTS;
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].name, "outline");
        assert_eq!(outs[0].ty, PortType::Array(pt_layout));
        assert_eq!(outs[1].name, "edges");
        assert_eq!(outs[1].ty, PortType::Array(edge_layout));
    }

    #[test]
    fn output_capacity_scales_with_max_rings() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ArrayReplicatePolylineRings::new();

        let default_params = ParamValues::default();
        let inputs = [
            ("outline", 64_u32),
            ("edges", 64_u32),
            ("scales", 16_u32),
        ];
        // Default max_rings = 16, outline cap 64 → output 1024.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "outline", &default_params, &inputs),
            Some(64 * 16),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &default_params, &inputs),
            Some(64 * 16),
        );

        // Clamped to REPLICATE_MAX_RINGS = 32 when the param exceeds it.
        let mut huge = ParamValues::default();
        huge.insert(std::borrow::Cow::Borrowed("max_rings"), ParamValue::Float(128.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "outline", &huge, &inputs),
            Some(64 * REPLICATE_MAX_RINGS),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ArrayReplicatePolylineRings::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.repeat_outline");
    }
}
