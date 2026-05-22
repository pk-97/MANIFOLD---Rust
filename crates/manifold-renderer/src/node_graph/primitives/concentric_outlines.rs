//! `node.concentric_outlines` — stack K scaled copies of a polygon
//! outline into a single line-rendering buffer pair.
//!
//! Takes one polygon's outline + edge topology and emits `ring_count`
//! concentric copies at scales driven by an `expansion` scalar.
//! Ring `i` scale = `(frac(expansion) + i) * ring_spacing`, so as
//! `expansion` grows over time the whole stack drifts outward and a
//! new ring emerges at the centre (radius 0) every integer crossing
//! of expansion. Innermost slot ranges [0, ring_spacing); outermost
//! ranges [(K-1)*spacing, K*spacing). Pick `ring_count` large enough
//! that K*spacing exceeds the visible viewport's corner distance
//! (≈0.707 in centred [-0.5, 0.5] coords) so the outermost slot is
//! always off-screen — that's where the slot-cycle wrap happens, and
//! making the wrap invisible is what keeps the animation smooth.
//!
//! Use case: the decomposition target for the legacy ConcentricTunnel
//! generator. Wire `node.polygon_shape → node.concentric_outlines →
//! node.render_lines` to draw concentric polygon rings (Triangle,
//! Square, Pentagon, Hexagon, or any N-gon polygon_shape produces).
//!
//! All work is CPU-side: input outline + edges are already CPU-written
//! by polygon_shape into shared MTLBuffers, and `node.render_lines`
//! reads the output edges CPU-side downstream. No GPU dispatch.
//!
//! Sentinel edges in the input (polygon_shape's padding beyond the
//! active N sides) propagate as sentinels in every output ring — they
//! never resolve to a drawn line. Inactive outline vertices stay at
//! origin, scaled to origin, harmless.

use crate::generators::mesh_common::{EdgePair, LinePoint};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Largest legal ring count. Sets the multiplier on the input outline
/// capacity to derive the output buffer size at plan time. 32 covers
/// every realistic "tunnel" depth — beyond that, distant rings fall
/// off-screen at any plausible `ring_spacing`.
pub const CONCENTRIC_MAX_RING_COUNT: u32 = 32;

/// Stack-allocated scratch size per ring iteration in `run()`. Must be
/// at least as large as the upstream producer's outline / edges
/// capacity. Sized to handle `polygon_shape`'s 64-vertex output with
/// margin; bump if a wider producer ships.
const CONCENTRIC_INLINE_SCRATCH: usize = 128;

crate::primitive! {
    name: ConcentricOutlines,
    type_id: "node.concentric_outlines",
    purpose: "Stack `ring_count` scaled copies of a polygon outline into one Array<LinePoint> + Array<EdgePair> pair, with ring `i` at scale `(frac(expansion) + i) * ring_spacing`. Drives the concentric-rings tunnel look from a single polygon source: wire node.polygon_shape's outline + edges in, drive `expansion` from `beat / beats_per_ring` (or any time-varying scalar), and feed the outputs into node.render_lines. As expansion grows past each integer step the stack drifts outward and a new ring emerges from the centre (slot 0 starts at radius 0).",
    inputs: {
        outline: Array(LinePoint) required,
        edges: Array(EdgePair) required,
        // Port-shadows-param: wire beat / time / LFO to animate the
        // tunnel. Param is the static fallback.
        expansion: ScalarF32 optional,
        // Port-shadows-param: wire an outer-card scale slider (or
        // anything else dynamic) to drive ring spacing.
        ring_spacing: ScalarF32 optional,
    },
    outputs: {
        outline: Array(LinePoint),
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: "expansion",
            label: "Expansion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1_000_000.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ring_count",
            label: "Ring Count",
            ty: ParamType::Int,
            // 16 covers the visible viewport at ring_spacing ≥ 0.05
            // (16 * 0.05 = 0.8 > viewport-corner 0.707), so the outermost
            // slot's cycle-wrap stays off-screen. Bump higher if a preset
            // dials ring_spacing below 0.05.
            default: ParamValue::Int(16),
            range: Some((1.0, CONCENTRIC_MAX_RING_COUNT as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: "ring_spacing",
            label: "Ring Spacing",
            ty: ParamType::Float,
            // 0.12 puts ~6 rings inside the visible viewport at
            // ring_count=16; outer half stays off-screen so the
            // slot-cycle wrap is invisible.
            default: ParamValue::Float(0.12),
            range: Some((0.001, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output buffer sizes scale with `ring_count` × the upstream Array capacities (e.g. polygon_shape's 64-vertex outline × 16 rings = 1024 LinePoints output). Each ring offset is `(frac(expansion) + i) * ring_spacing`; slot 0 ranges from radius 0 to `ring_spacing` (rings emerge from the centre and grow out), slot K-1 ranges from `(K-1)*spacing` to `K*spacing`. Edges from the input are copied per ring with vertex indices shifted by ring_index × input_capacity, so each ring is a self-contained closed loop. Sentinel edges (EdgePair::SENTINEL) in the input stay as sentinels in every output ring — polygon_shape's inactive-side padding never resolves to a drawn line. Pick `ring_count` so K*spacing > 0.707 (viewport corner) and the slot-cycle wrap stays off-screen.",
    examples: [],
    picker: { label: "Concentric Outlines", category: Atom },
}

impl Primitive for ConcentricOutlines {
    /// Output capacities are the input outline / edges capacity
    /// multiplied by `ring_count`. The plan-time `_input_capacities`
    /// vector carries the producer's capacities for each named port.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let ring_count = match params.get("ring_count") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 8,
        }
        .min(CONCENTRIC_MAX_RING_COUNT);

        match port_name {
            "outline" => input_capacities
                .iter()
                .find(|(p, _)| *p == "outline")
                .map(|(_, n)| n * ring_count),
            "edges" => input_capacities
                .iter()
                .find(|(p, _)| *p == "edges")
                .map(|(_, n)| n * ring_count),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let expansion = ctx.scalar_or_param("expansion", 0.0);

        let ring_count = match ctx.params.get("ring_count") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 8,
        }
        .min(CONCENTRIC_MAX_RING_COUNT);

        let ring_spacing = ctx.scalar_or_param("ring_spacing", 0.12);

        // ── Resolve buffers ──
        let Some(outline_in) = ctx.inputs.array("outline") else {
            return;
        };
        let Some(edges_in) = ctx.inputs.array("edges") else {
            return;
        };
        let Some(outline_out) = ctx.outputs.array("outline") else {
            log::warn!(
                "node.concentric_outlines: no GpuBuffer bound to output port `outline` — \
                 the chain build did not pre-allocate the Array<LinePoint> output."
            );
            return;
        };
        let Some(edges_out) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.concentric_outlines: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output."
            );
            return;
        };

        let in_outline_cap =
            (outline_in.size / std::mem::size_of::<LinePoint>() as u64) as u32;
        let in_edges_cap = (edges_in.size / std::mem::size_of::<EdgePair>() as u64) as u32;
        let out_outline_cap =
            (outline_out.size / std::mem::size_of::<LinePoint>() as u64) as u32;
        let out_edges_cap = (edges_out.size / std::mem::size_of::<EdgePair>() as u64) as u32;
        if in_outline_cap == 0 || in_edges_cap == 0 || out_outline_cap == 0 || out_edges_cap == 0
        {
            return;
        }

        // ── Read input via mapped_ptr ──
        // Shared MTLBuffers expose a persistent CPU-visible pointer
        // (polygon_shape writes through this same pointer). Safe to
        // read because the executor walks primitives sequentially on
        // the content thread — polygon_shape's write has already
        // completed by the time we run.
        let outline_in_ptr = outline_in
            .mapped_ptr()
            .expect("concentric_outlines: outline input must be shared-memory");
        let edges_in_ptr = edges_in
            .mapped_ptr()
            .expect("concentric_outlines: edges input must be shared-memory");
        let in_outline: &[LinePoint] = unsafe {
            std::slice::from_raw_parts(
                outline_in_ptr as *const LinePoint,
                in_outline_cap as usize,
            )
        };
        let in_edges: &[EdgePair] = unsafe {
            std::slice::from_raw_parts(edges_in_ptr as *const EdgePair, in_edges_cap as usize)
        };

        // ── Scratch + ring scales ──
        // Frac of expansion drives the slot-cycle. Slot 0 emerges from
        // the centre (radius 0 at frac=0) and grows to one ring_spacing
        // out as frac → 1, then wraps back to 0 as the next ring
        // emerges. Higher slots cycle through their respective bands.
        let frac_expansion = expansion - expansion.floor();

        // Output is at most CONCENTRIC_MAX_RING_COUNT × input capacity
        // each. With polygon_shape's 32-slot outline and 32 rings max,
        // that's 1024 entries × 8B = 8 KB. Cheap stack allocation.
        // We grow the scratch lazily per call to avoid pre-allocating
        // the max if the caller uses a small ring_count.
        let outline_items = (ring_count as usize) * (in_outline_cap as usize);
        let edges_items = (ring_count as usize) * (in_edges_cap as usize);

        let outline_write_count = (out_outline_cap as usize).min(outline_items);
        let edges_write_count = (out_edges_cap as usize).min(edges_items);

        // Build the per-ring scaled outline slice-by-slice (writing
        // straight into the output buffer ring by ring; no big single
        // scratch buffer needed because each ring's contribution is
        // a contiguous run in the output buffer).
        for i in 0..ring_count {
            let scale = (frac_expansion + i as f32) * ring_spacing;
            let ring_outline_offset = (i as usize) * (in_outline_cap as usize);
            let ring_edges_offset = (i as usize) * (in_edges_cap as usize);

            // Skip this ring entirely if it falls past the output cap.
            if ring_outline_offset >= outline_write_count {
                break;
            }

            // Scale every input vertex into a stack scratch sized to
            // the input capacity (small — <=32 typically). Avoids
            // allocating a Vec per call.
            let outline_chunk = (outline_write_count - ring_outline_offset)
                .min(in_outline_cap as usize);
            let mut outline_scratch = [LinePoint { xy: [0.0, 0.0] }; CONCENTRIC_INLINE_SCRATCH];
            assert!(
                outline_chunk <= outline_scratch.len(),
                "concentric_outlines: input outline capacity {} exceeds inline scratch size {}; bump CONCENTRIC_INLINE_SCRATCH if a producer is wider",
                in_outline_cap,
                CONCENTRIC_INLINE_SCRATCH,
            );
            for j in 0..outline_chunk {
                let p = in_outline[j].xy;
                outline_scratch[j] = LinePoint {
                    xy: [p[0] * scale, p[1] * scale],
                };
            }

            // Edges: shift indices by ring offset, propagate sentinels.
            let edges_chunk = if ring_edges_offset >= edges_write_count {
                0
            } else {
                (edges_write_count - ring_edges_offset).min(in_edges_cap as usize)
            };
            let mut edges_scratch = [EdgePair::SENTINEL; CONCENTRIC_INLINE_SCRATCH];
            assert!(
                edges_chunk <= edges_scratch.len(),
                "concentric_outlines: input edges capacity {} exceeds inline scratch size {}",
                in_edges_cap,
                CONCENTRIC_INLINE_SCRATCH,
            );
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

            // Safety: shared-memory MTLBuffers prebound by the chain
            // build, offsets clamped above to the buffer capacities,
            // no GPU pass races these writes (sequential executor on
            // the content thread).
            unsafe {
                outline_out.write(
                    (ring_outline_offset as u64) * std::mem::size_of::<LinePoint>() as u64,
                    bytemuck::cast_slice(&outline_scratch[..outline_chunk]),
                );
                if edges_chunk > 0 {
                    edges_out.write(
                        (ring_edges_offset as u64) * std::mem::size_of::<EdgePair>() as u64,
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
    use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

    #[test]
    fn declares_two_array_inputs_two_scalar_inputs_and_two_array_outputs() {
        let line_layout = ArrayType {
            item_size: std::mem::size_of::<LinePoint>() as u32,
            item_align: std::mem::align_of::<LinePoint>() as u32,
        };
        let edge_layout = ArrayType {
            item_size: std::mem::size_of::<EdgePair>() as u32,
            item_align: std::mem::align_of::<EdgePair>() as u32,
        };

        assert_eq!(ConcentricOutlines::TYPE_ID, "node.concentric_outlines");
        let ins = ConcentricOutlines::INPUTS;
        assert_eq!(ins.len(), 4);
        assert_eq!(ins[0].name, "outline");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Array(line_layout));
        assert_eq!(ins[1].name, "edges");
        assert!(ins[1].required);
        assert_eq!(ins[1].ty, PortType::Array(edge_layout));
        assert_eq!(ins[2].name, "expansion");
        assert!(!ins[2].required);
        assert_eq!(ins[2].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[3].name, "ring_spacing");
        assert!(!ins[3].required);
        assert_eq!(ins[3].ty, PortType::Scalar(ScalarType::F32));

        let outs = ConcentricOutlines::OUTPUTS;
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].name, "outline");
        assert_eq!(outs[0].ty, PortType::Array(line_layout));
        assert_eq!(outs[1].name, "edges");
        assert_eq!(outs[1].ty, PortType::Array(edge_layout));
    }

    #[test]
    fn output_capacity_scales_with_ring_count() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ConcentricOutlines::new();

        // Default ring_count = 8, input capacity 32 → output capacity 256.
        let default_params = ParamValues::default();
        let inputs = [("outline", 32u32), ("edges", 32u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "outline", &default_params, &inputs),
            Some(32 * 8)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &default_params, &inputs),
            Some(32 * 8)
        );

        // Custom ring_count = 4
        let mut custom = ParamValues::default();
        custom.insert("ring_count", ParamValue::Int(4));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "outline", &custom, &inputs),
            Some(32 * 4)
        );

        // Clamped to CONCENTRIC_MAX_RING_COUNT
        let mut huge = ParamValues::default();
        huge.insert("ring_count", ParamValue::Int(128));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "outline", &huge, &inputs),
            Some(32 * CONCENTRIC_MAX_RING_COUNT)
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ConcentricOutlines::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.concentric_outlines");
    }
}

#[cfg(test)]
mod gpu_tests {
    //! Hardware parity test — runs concentric_outlines through the
    //! graph executor with polygon_shape upstream and reads back the
    //! stacked outline + edges buffers. Confirms (a) the scaling math
    //! matches the documented formula, (b) edges are shifted per ring,
    //! (c) input sentinels propagate as output sentinels.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::mesh_common::{EdgePair, LinePoint};
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };
    use crate::node_graph::primitives::{ConcentricOutlines, PolygonShape};

    use super::CONCENTRIC_MAX_RING_COUNT;
    use crate::node_graph::primitives::polygon_shape::POLYGON_MAX_SIDES;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    /// Build `polygon_shape → concentric_outlines` with given params,
    /// run one frame, read back the stacked outline + edges.
    fn run_chain(
        n_sides: i32,
        size: f32,
        ring_count: i32,
        ring_spacing: f32,
        expansion: f32,
    ) -> (Vec<LinePoint>, Vec<EdgePair>) {
        use crate::node_graph::primitives::polygon_shape::POLYGON_MAX_MESH_VERTS;
        use crate::generators::mesh_common::MeshVertex;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let poly = g.add_node(Box::new(PolygonShape::new()));
        // polygon_shape reads `n_sides` via `scalar_or_param`, which
        // only matches ParamValue::Float — passing Int(N) here would
        // silently fall back to the default 4.0. (Live presets set
        // n_sides via the EnumRound/Float convert path, which encodes
        // Float; the ConcentricTunnel preset does the same.)
        g.set_param(poly, "n_sides", ParamValue::Float(n_sides as f32))
            .unwrap();
        g.set_param(poly, "size", ParamValue::Float(size)).unwrap();
        g.set_param(poly, "rotation", ParamValue::Float(0.0)).unwrap();

        let stack = g.add_node(Box::new(ConcentricOutlines::new()));
        g.set_param(stack, "ring_count", ParamValue::Int(ring_count)).unwrap();
        g.set_param(stack, "ring_spacing", ParamValue::Float(ring_spacing))
            .unwrap();
        g.set_param(stack, "expansion", ParamValue::Float(expansion))
            .unwrap();

        g.connect((poly, "outline"), (stack, "outline")).unwrap();
        g.connect((poly, "edges"), (stack, "edges")).unwrap();

        let plan = compile(&g).unwrap();
        let r_outline_in = output_resource(&plan, poly, "outline");
        let r_edges_in = output_resource(&plan, poly, "edges");
        let r_mesh_in = output_resource(&plan, poly, "mesh");
        let r_outline = output_resource(&plan, stack, "outline");
        let r_edges = output_resource(&plan, stack, "edges");

        // Intermediate buffers (poly outputs → stack inputs).
        // The bare Graph + Executor path doesn't auto-allocate Array
        // resources; `JsonGraphGenerator` does. For this gpu_test we
        // pre-bind every Array resource explicitly so polygon_shape
        // has somewhere to write before concentric_outlines reads.
        let poly_outline_buf = device.create_buffer_shared(
            (POLYGON_MAX_SIDES as u64) * std::mem::size_of::<LinePoint>() as u64,
        );
        let poly_edges_buf = device.create_buffer_shared(
            (POLYGON_MAX_SIDES as u64) * std::mem::size_of::<EdgePair>() as u64,
        );
        let poly_mesh_buf = device.create_buffer_shared(
            (POLYGON_MAX_MESH_VERTS as u64) * std::mem::size_of::<MeshVertex>() as u64,
        );

        let outline_bytes = (POLYGON_MAX_SIDES as u64)
            * (ring_count as u64)
            * std::mem::size_of::<LinePoint>() as u64;
        let edges_bytes = (POLYGON_MAX_SIDES as u64)
            * (ring_count as u64)
            * std::mem::size_of::<EdgePair>() as u64;
        let outline_buf = device.create_buffer_shared(outline_bytes);
        let edges_buf = device.create_buffer_shared(edges_bytes);

        let mut backend = MetalBackend::new(&device, 1, 1, format);
        let _ = backend.pre_bind_array(r_outline_in, poly_outline_buf);
        let _ = backend.pre_bind_array(r_edges_in, poly_edges_buf);
        let _ = backend.pre_bind_array(r_mesh_in, poly_mesh_buf);
        let outline_slot = backend.pre_bind_array(r_outline, outline_buf);
        let edges_slot = backend.pre_bind_array(r_edges, edges_buf);

        let mut native_enc = device.create_encoder("concentric-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let outline_b = exec
            .backend()
            .array_buffer(outline_slot)
            .expect("outline buffer retained");
        let edges_b = exec
            .backend()
            .array_buffer(edges_slot)
            .expect("edges buffer retained");

        let o_ptr = outline_b.mapped_ptr().expect("shared outline buffer");
        let e_ptr = edges_b.mapped_ptr().expect("shared edges buffer");
        let o_slice = unsafe {
            std::slice::from_raw_parts(o_ptr as *const u8, outline_bytes as usize)
        };
        let e_slice = unsafe {
            std::slice::from_raw_parts(e_ptr as *const u8, edges_bytes as usize)
        };
        let o: Vec<LinePoint> = bytemuck::cast_slice::<u8, LinePoint>(o_slice).to_vec();
        let e: Vec<EdgePair> = bytemuck::cast_slice::<u8, EdgePair>(e_slice).to_vec();
        (o, e)
    }

    #[test]
    fn ring_scales_match_expansion_formula() {
        // Triangle (n=3), size=1.0, ring_count=4, ring_spacing=0.1,
        // expansion=0 → ring i should scale by `i * 0.1` =
        // 0.0, 0.1, 0.2, 0.3 (slot 0 is collapsed at the origin —
        // a new ring just emerged).
        let (outline, _) = run_chain(3, 1.0, 4, 0.1, 0.0);
        let expected_scales = [0.0_f32, 0.1, 0.2, 0.3];
        for (i, &expected_scale) in expected_scales.iter().enumerate() {
            // Vertex 0 of polygon_shape sits at (size, 0) → (1.0, 0).
            // After scaling by expected_scale: (expected_scale, 0).
            // Ring i's vertex 0 is at output offset i * POLYGON_MAX_SIDES.
            let v0 = outline[i * (POLYGON_MAX_SIDES as usize)];
            assert!(
                (v0.xy[0] - expected_scale).abs() < 1e-5,
                "ring {i} vertex 0 x: got {}, expected {}",
                v0.xy[0],
                expected_scale,
            );
            assert!(v0.xy[1].abs() < 1e-5);
        }
    }

    #[test]
    fn edge_indices_shift_by_ring_offset() {
        // Triangle (n=3): each ring has 3 active edges in a closed loop
        // (0→1, 1→2, 2→0). After ring-stacking, edge indices are
        // offset by ring_index × POLYGON_MAX_SIDES (the producer's
        // outline capacity, which is the offset stride for indices).
        let (_, edges) = run_chain(3, 1.0, 3, 0.1, 0.0);
        let stride = POLYGON_MAX_SIDES;
        let triangle_edges: &[(u32, u32)] = &[(0, 1), (1, 2), (2, 0)];
        for ring in 0..3u32 {
            let base = (ring * stride) as usize;
            for (i, &(a, b)) in triangle_edges.iter().enumerate() {
                let e = edges[base + i];
                assert_eq!(e.a, a + ring * stride, "ring {ring} edge {i}.a");
                assert_eq!(e.b, b + ring * stride, "ring {ring} edge {i}.b");
            }
        }
    }

    #[test]
    fn sentinel_edges_propagate_to_all_rings() {
        // Triangle has 3 active edges. Edges 3..32 in polygon_shape's
        // output are sentinels. Every output ring should preserve
        // sentinels at positions 3..32 (no index shifting because
        // sentinel = u32::MAX is a special value).
        let (_, edges) = run_chain(3, 1.0, 3, 0.1, 0.0);
        for ring in 0..3 {
            let base = ring * (POLYGON_MAX_SIDES as usize);
            for j in 3..(POLYGON_MAX_SIDES as usize) {
                let e = edges[base + j];
                assert_eq!(
                    e.a,
                    u32::MAX,
                    "ring {ring} edge {j} should be sentinel a, got {}",
                    e.a,
                );
                assert_eq!(e.b, u32::MAX, "ring {ring} edge {j} should be sentinel b");
            }
        }
    }

    #[test]
    fn expansion_frac_offsets_ring_scale() {
        // expansion = 0.5, ring_count = 4, ring_spacing = 0.1
        // Ring i scale = (0.5 + i) * 0.1 →
        // 0.05, 0.15, 0.25, 0.35 (slot 0 mid-emergence).
        let (outline, _) = run_chain(4, 1.0, 4, 0.1, 0.5);
        let expected_scales = [0.05_f32, 0.15, 0.25, 0.35];
        for (i, &expected_scale) in expected_scales.iter().enumerate() {
            let v0 = outline[i * (POLYGON_MAX_SIDES as usize)];
            assert!(
                (v0.xy[0] - expected_scale).abs() < 1e-5,
                "ring {i} vertex 0 x: got {}, expected {}",
                v0.xy[0],
                expected_scale,
            );
        }
    }

    #[test]
    fn ring_count_bound_by_max() {
        // Make sure the macro-declared constant matches the runtime.
        // Catches future drift if CONCENTRIC_MAX_RING_COUNT is changed
        // without updating the param range or output capacity calc.
        assert_eq!(CONCENTRIC_MAX_RING_COUNT, 32);
    }
}
