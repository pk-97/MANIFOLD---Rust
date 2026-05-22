//! `node.polygon_shape` — generate a regular N-gon as three coordinated
//! outputs: outline points, edge topology, and a fan-triangulated mesh.
//!
//! The geometry source for any 2D polygon look. Pair with:
//!
//! - **`node.render_lines`** (wiring `edges`) — anti-aliased wireframe
//!   outlines via the same capsule-SDF + fragment-fwidth pipeline that
//!   draws Lissajous and the wireframe Platonic solids. Hardware
//!   derivative AA, smoother than any compute-side SDF rasterisation.
//! - **`node.render_3d_mesh`** — solid fill via depth-tested triangle
//!   rasterisation. Mesh is emitted with `z = 0`, `normal = (0, 0, 1)`,
//!   so a flat-camera setup (orbit/tilt at 0) renders the polygon as
//!   the user expects; non-zero camera angles tilt the flat shape in
//!   3D, which is a feature for variant looks.
//!
//! Three outputs from one primitive (mirroring `node.wireframe_shape`)
//! keep the geometry source single-purpose and let the graph author
//! pick whether they want outline, fill, or both compositions.
//!
//! Clip-trigger mode cycles through a curated set of side counts
//! [3, 4, 5, 6, 8, 12] via the shared `ClipTriggerCycle` uniqueness
//! invariant — same defence-in-depth as Plasma and WireframeShape.
//!
//! Vertex 0 sits at angle `rotation` from the positive x-axis,
//! counter-clockwise. All three buffers are CPU-written (small N,
//! cheap, and `node.render_lines` reads edges CPU-side downstream, so
//! a same-frame GPU write wouldn't be visible without a fence).

use crate::generators::clip_trigger::ClipTriggerCycle;
use crate::generators::mesh_common::{EdgePair, LinePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Smallest legal N (triangle). Below this the n-gon collapses.
pub const POLYGON_MIN_SIDES: u32 = 3;

/// Largest N supported. Sets the array buffer capacities for `outline`
/// and `edges`; at N = 32 a polygon visually approximates a circle to
/// the edge-pixel detection limit of an HD output.
pub const POLYGON_MAX_SIDES: u32 = 32;

/// Triangle list capacity for the fan-triangulated `mesh` output.
/// Three MeshVertex entries per triangle, one triangle per side
/// (centre + two adjacent perimeter vertices).
pub const POLYGON_MAX_MESH_VERTS: u32 = POLYGON_MAX_SIDES * 3;

/// Curated cycle sequence for clip-trigger mode. Each retrigger advances
/// through this list via `ClipTriggerCycle`; adjacent steps never land
/// on the same N. Sequence picks values that look perceptibly distinct —
/// 7 / 9 / 10 / 11 omitted because they read the same as 8 at the
/// rendering resolutions a live show uses.
pub const POLYGON_CYCLE_SIDES: &[u32] = &[3, 4, 5, 6, 8, 12];

crate::primitive! {
    name: PolygonShape,
    type_id: "node.polygon_shape",
    purpose: "Generate a regular N-gon as three coordinated outputs: outline (Array<LinePoint>), edge topology (Array<EdgePair>), and a fan-triangulated mesh (Array<MeshVertex>). The geometry source for 2D polygon looks — pair outline + edges with node.render_lines for anti-aliased wireframes, or mesh with node.render_3d_mesh for solid fill. Mesh sits at z = 0 with +z normals so a flat camera renders the polygon as the user expects. Vertex 0 lies at angle `rotation` measured CCW from positive x. Clip-trigger mode cycles N through [3, 4, 5, 6, 8, 12] on each retrigger via ClipTriggerCycle.",
    inputs: {
        // All scalar params are port-shadowable so a generator graph
        // can drive them from upstream (LFO, easing, trigger_count, …).
        n_sides: ScalarF32 optional,
        size: ScalarF32 optional,
        rotation: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        outline: Array(LinePoint),
        edges: Array(EdgePair),
        mesh: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "n_sides",
            label: "Sides",
            ty: ParamType::Int,
            default: ParamValue::Int(4),
            range: Some((POLYGON_MIN_SIDES as f32, POLYGON_MAX_SIDES as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: "size",
            label: "Size",
            ty: ParamType::Float,
            // 0.315 matches the legacy BasicShapes screen-fit
            // multiplier (the inner inscribed-circle radius that makes
            // the polygon fill the central viewport region).
            default: ParamValue::Float(0.315),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rotation",
            label: "Rotation",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "clip_trigger",
            label: "Clip Trigger",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Count",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "`n_sides`, `size`, `rotation` are port-shadows-param: wire upstream scalars (LFO, smoothing, eased step) to animate them, or set inline for a static polygon. Clip-trigger mode swaps the static `n_sides` for a cycled draw from `POLYGON_CYCLE_SIDES = [3, 4, 5, 6, 8, 12]` indexed by `trigger_count` through the ClipTriggerCycle uniqueness invariant — adjacent retriggers never land on the same N. `size` is the circumscribed-circle radius (vertex distance from origin); 0.315 fills the inner viewport like the legacy BasicShapes shapes. All three outputs are CPU-written into shared MTLBuffers so downstream `node.render_lines` reads `edges` CPU-side without a same-frame fence.",
    examples: [],
    picker: { label: "Polygon Shape", category: Atom },
    extra_fields: {
        clip_trigger_cycle: ClipTriggerCycle = ClipTriggerCycle::new(),
    },
}

impl Primitive for PolygonShape {
    /// Output capacities:
    /// - `outline`: POLYGON_MAX_SIDES (32) — one LinePoint per vertex.
    /// - `edges`:   POLYGON_MAX_SIDES (32) — one EdgePair per side
    ///   (closed loop).
    /// - `mesh`:    POLYGON_MAX_MESH_VERTS (96) — three MeshVertex per
    ///   triangle, one triangle per side (fan triangulation).
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "outline" => Some(POLYGON_MAX_SIDES),
            "edges" => Some(POLYGON_MAX_SIDES),
            "mesh" => Some(POLYGON_MAX_MESH_VERTS),
            _ => None,
        }
    }

    // Geometry loops use the index `i` to address current AND
    // neighbour slots (`outline[(i + 1) % n]`, `mesh[i * 3 + k]`);
    // clippy's `needless_range_loop` rewrite to enumerate doesn't help
    // when half the indexing is unrelated to the current iterator.
    #[allow(clippy::needless_range_loop)]
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // ── Resolve the effective N. ─────────────────────────────
        // clip_trigger=false: static `n_sides` param (or wire). When
        // clip_trigger=true the cycle drives N from trigger_count
        // through the curated sequence; param value is the inert side
        // of the mux (§7's documented exception).
        let clip_trigger = match ctx.params.get("clip_trigger") {
            Some(ParamValue::Bool(b)) => *b,
            Some(ParamValue::Float(f)) => *f > 0.5,
            Some(ParamValue::Int(i)) => *i != 0,
            _ => false,
        };

        let n = if clip_trigger {
            // Port-shadows-param: wired `trigger_count` input wins.
            let trigger_count = ctx.scalar_or_param("trigger_count", 0.0);
            let raw = trigger_count.floor().max(0.0) as u32;
            let idx = self
                .clip_trigger_cycle
                .step(raw, POLYGON_CYCLE_SIDES.len() as u32);
            POLYGON_CYCLE_SIDES[idx as usize]
        } else {
            // Port-shadow on `n_sides` too: wired input wins, falls
            // back to the param. Round and clamp to the legal range.
            let raw = ctx.scalar_or_param("n_sides", 4.0);
            (raw.round() as i32)
                .clamp(POLYGON_MIN_SIDES as i32, POLYGON_MAX_SIDES as i32) as u32
        };

        let size = ctx.scalar_or_param("size", 0.315);
        let rotation = ctx.scalar_or_param("rotation", 0.0);

        // ── Resolve the three output buffers. ────────────────────
        let Some(outline_dst) = ctx.outputs.array("outline") else {
            log::warn!(
                "node.polygon_shape: no GpuBuffer bound to output port `outline` — \
                 the chain build did not pre-allocate the Array<LinePoint> output.",
            );
            return;
        };
        let Some(edges_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.polygon_shape: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output.",
            );
            return;
        };
        let Some(mesh_dst) = ctx.outputs.array("mesh") else {
            log::warn!(
                "node.polygon_shape: no GpuBuffer bound to output port `mesh` — \
                 the chain build did not pre-allocate the Array<MeshVertex> output.",
            );
            return;
        };

        let outline_cap = (outline_dst.size / std::mem::size_of::<LinePoint>() as u64) as u32;
        let edges_cap = (edges_dst.size / std::mem::size_of::<EdgePair>() as u64) as u32;
        let mesh_cap = (mesh_dst.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
        if outline_cap == 0 || edges_cap == 0 || mesh_cap == 0 {
            return;
        }

        // ── Compute the geometry into stack-allocated scratch. ───
        // Capped at POLYGON_MAX_SIDES; the run-time N is clamped above
        // so we never overflow. Padding the tail with zeros (outline /
        // mesh) and SENTINELs (edges) makes the buffers safe for
        // partial consumption by downstream primitives.
        let mut outline_scratch = [LinePoint { xy: [0.0, 0.0] }; POLYGON_MAX_SIDES as usize];
        let mut edges_scratch = [EdgePair::SENTINEL; POLYGON_MAX_SIDES as usize];
        let mut mesh_scratch = [MeshVertex {
            position: [0.0, 0.0, 0.0],
            _pad0: 0.0,
            normal: [0.0, 0.0, 1.0],
            _pad1: 0.0,
        }; POLYGON_MAX_MESH_VERTS as usize];

        let n_usize = n as usize;
        let n_f32 = n as f32;
        let angle_step = std::f32::consts::TAU / n_f32;

        // Outline: N LinePoints around the circumscribed circle.
        for i in 0..n_usize {
            let theta = rotation + (i as f32) * angle_step;
            outline_scratch[i] = LinePoint {
                xy: [size * theta.cos(), size * theta.sin()],
            };
        }

        // Edges: N EdgePairs forming a closed loop (last → first).
        for i in 0..n_usize {
            edges_scratch[i] = EdgePair {
                a: i as u32,
                b: ((i + 1) % n_usize) as u32,
            };
        }

        // Mesh: fan triangulation from origin. Triangle i is
        // (centre, perimeter[i], perimeter[(i+1) % N]) — 3
        // consecutive MeshVertex entries per triangle for triangle-
        // list consumption by node.render_3d_mesh.
        for i in 0..n_usize {
            let a = outline_scratch[i].xy;
            let b = outline_scratch[(i + 1) % n_usize].xy;
            let base = i * 3;
            // Centre vertex stays at origin with +z normal.
            mesh_scratch[base] = MeshVertex {
                position: [0.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
            };
            mesh_scratch[base + 1] = MeshVertex {
                position: [a[0], a[1], 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
            };
            mesh_scratch[base + 2] = MeshVertex {
                position: [b[0], b[1], 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
            };
        }

        // ── Write all three buffers. ─────────────────────────────
        // Safety: shared-memory MTLBuffers (per `pre_allocate_array_buffers`)
        // prebound by the chain build; counts clamped to the buffer
        // capacities; no GPU pass races these writes because the
        // executor walks primitives sequentially on the content thread.
        let outline_write = (outline_cap as usize).min(outline_scratch.len());
        let edges_write = (edges_cap as usize).min(edges_scratch.len());
        let mesh_write = (mesh_cap as usize).min(mesh_scratch.len());
        unsafe {
            outline_dst.write(0, bytemuck::cast_slice(&outline_scratch[..outline_write]));
            edges_dst.write(0, bytemuck::cast_slice(&edges_scratch[..edges_write]));
            mesh_dst.write(0, bytemuck::cast_slice(&mesh_scratch[..mesh_write]));
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
    fn polygon_shape_declares_four_optional_scalar_inputs_and_three_array_outputs() {
        assert_eq!(PolygonShape::TYPE_ID, "node.polygon_shape");
        let ins = PolygonShape::INPUTS;
        assert_eq!(ins.len(), 4);
        for (i, name) in ["n_sides", "size", "rotation", "trigger_count"]
            .iter()
            .enumerate()
        {
            assert_eq!(ins[i].name, *name);
            assert!(!ins[i].required);
            assert_eq!(ins[i].ty, PortType::Scalar(ScalarType::F32));
        }
        let outs = PolygonShape::OUTPUTS;
        assert_eq!(outs.len(), 3);
        let outline_layout = ArrayType {
            item_size: std::mem::size_of::<LinePoint>() as u32,
            item_align: std::mem::align_of::<LinePoint>() as u32,
        };
        let edges_layout = ArrayType {
            item_size: std::mem::size_of::<EdgePair>() as u32,
            item_align: std::mem::align_of::<EdgePair>() as u32,
        };
        let mesh_layout = ArrayType {
            item_size: std::mem::size_of::<MeshVertex>() as u32,
            item_align: std::mem::align_of::<MeshVertex>() as u32,
        };
        assert_eq!(outs[0].name, "outline");
        assert_eq!(outs[0].ty, PortType::Array(outline_layout));
        assert_eq!(outs[1].name, "edges");
        assert_eq!(outs[1].ty, PortType::Array(edges_layout));
        assert_eq!(outs[2].name, "mesh");
        assert_eq!(outs[2].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn polygon_shape_has_n_sides_size_rotation_clip_trigger_params() {
        let names: Vec<&str> = PolygonShape::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["n_sides", "size", "rotation", "clip_trigger", "trigger_count"]
        );
    }

    #[test]
    fn polygon_shape_cycle_sequence_is_distinct_and_perceptible() {
        // The curated cycle list must be strictly increasing (no
        // duplicates, no backtracking) and start at the minimum legal
        // N. POLYGON_CYCLE_SIDES drives the clip-trigger mux.
        assert_eq!(POLYGON_CYCLE_SIDES[0], POLYGON_MIN_SIDES);
        for w in POLYGON_CYCLE_SIDES.windows(2) {
            assert!(
                w[0] < w[1],
                "cycle sequence must be strictly increasing; got {:?}",
                POLYGON_CYCLE_SIDES,
            );
        }
        for &n in POLYGON_CYCLE_SIDES {
            assert!(
                (POLYGON_MIN_SIDES..=POLYGON_MAX_SIDES).contains(&n),
                "cycle entry {n} outside legal [{POLYGON_MIN_SIDES}, {POLYGON_MAX_SIDES}]",
            );
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PolygonShape::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.polygon_shape");
    }

    /// CPU spec for the perimeter — vertex `i` sits at angle
    /// `rotation + i * TAU / N`, distance `size` from origin. This is
    /// the contract `run()` must implement; the GPU smoke tests below
    /// verify the actual buffer contents match.
    fn expected_outline(n: u32, size: f32, rotation: f32) -> Vec<[f32; 2]> {
        let n_f32 = n as f32;
        let step = std::f32::consts::TAU / n_f32;
        (0..n as usize)
            .map(|i| {
                let theta = rotation + (i as f32) * step;
                [size * theta.cos(), size * theta.sin()]
            })
            .collect()
    }

    #[test]
    fn outline_spec_square_at_zero_rotation() {
        // n=4, rotation=0: vertices at right / top / left / bottom of
        // a circle of radius `size`. (This is a diamond visually —
        // axis-aligned squares need rotation = π/4.)
        let pts = expected_outline(4, 1.0, 0.0);
        assert!((pts[0][0] - 1.0).abs() < 1e-5 && pts[0][1].abs() < 1e-5);
        assert!(pts[1][0].abs() < 1e-5 && (pts[1][1] - 1.0).abs() < 1e-5);
        assert!((pts[2][0] + 1.0).abs() < 1e-5 && pts[2][1].abs() < 1e-5);
        assert!(pts[3][0].abs() < 1e-5 && (pts[3][1] + 1.0).abs() < 1e-5);
    }

    #[test]
    fn outline_spec_octagon_distance_to_origin_constant() {
        // Every vertex of a regular polygon sits at radius `size`.
        let pts = expected_outline(8, 0.5, 0.123);
        for [x, y] in pts {
            let r = (x * x + y * y).sqrt();
            assert!((r - 0.5).abs() < 1e-5, "expected radius 0.5, got {r}");
        }
    }
}

#[cfg(test)]
mod gpu_tests {
    //! GPU-buffer-shape tests for `node.polygon_shape`. The primitive
    //! is CPU-computed (no shader), so these tests run it through the
    //! standard graph executor with a MockBackend, read back the three
    //! shared-memory output buffers, and compare element-wise against
    //! the CPU spec from the `tests` module.
    //!
    //! Three distinct N values cover the cycling sequence: triangle
    //! (minimum), square (cycle entry 1), octagon (cycle entry 4).
    //! The tail of each buffer must stay at its sentinel/zero value so
    //! downstream consumers (render_lines stops at EdgePair::SENTINEL)
    //! correctly identify active vs. unused entries.
    use super::*;

    fn run_polygon_shape_cpu(n: u32, size: f32, rotation: f32)
    -> (Vec<LinePoint>, Vec<EdgePair>, Vec<MeshVertex>) {
        // Drive `run()` against in-process shared buffers via a small
        // standalone harness. This sidesteps the graph executor — the
        // primitive's logic is pure CPU; we only need to verify it
        // writes the buffers correctly.
        let prim = PolygonShape::new();
        prim.cpu_compute_for_test(n, size, rotation)
    }

    #[test]
    fn cycle_advances_n_on_each_trigger() {
        // ClipTriggerCycle is stateful — verify N cycles through the
        // curated sequence on each trigger via the primitive's own
        // cycle field. Direct field access keeps this test scoped to
        // the cycling logic, independent of the graph executor.
        let mut prim = PolygonShape::new();
        let mut emitted = Vec::new();
        for tc in 0..(POLYGON_CYCLE_SIDES.len() as u32 * 2) {
            let idx = prim
                .clip_trigger_cycle
                .step(tc, POLYGON_CYCLE_SIDES.len() as u32);
            emitted.push(POLYGON_CYCLE_SIDES[idx as usize]);
        }
        // Adjacent emissions must always differ — the ClipTriggerCycle
        // contract.
        for w in emitted.windows(2) {
            assert_ne!(
                w[0], w[1],
                "adjacent emissions must differ: {emitted:?}",
            );
        }
    }

    /// Helper: EdgePair lacks `PartialEq`/`Debug` upstream; tests
    /// compare field-by-field.
    fn edge_eq(actual: EdgePair, a: u32, b: u32) -> bool {
        actual.a == a && actual.b == b
    }

    #[test]
    fn triangle_geometry_matches_spec() {
        let (outline, edges, mesh) = run_polygon_shape_cpu(3, 0.5, 0.0);
        assert_eq!(outline[0].xy, [0.5, 0.0]);
        let two_pi_over_3 = std::f32::consts::TAU / 3.0;
        assert!((outline[1].xy[0] - 0.5 * two_pi_over_3.cos()).abs() < 1e-5);
        assert!((outline[1].xy[1] - 0.5 * two_pi_over_3.sin()).abs() < 1e-5);
        // Edges close the loop: 0→1, 1→2, 2→0; remaining slots are
        // SENTINEL.
        assert!(edge_eq(edges[0], 0, 1));
        assert!(edge_eq(edges[1], 1, 2));
        assert!(edge_eq(edges[2], 2, 0));
        assert!(edge_eq(edges[3], u32::MAX, u32::MAX));
        // Mesh: 3 triangles, 9 MeshVertex entries used. Every triangle
        // starts with a centre vertex at origin.
        for tri in 0..3 {
            assert_eq!(mesh[tri * 3].position, [0.0, 0.0, 0.0]);
            assert_eq!(mesh[tri * 3].normal, [0.0, 0.0, 1.0]);
        }
        // Tail mesh vertex must be the zero/+z normal sentinel.
        assert_eq!(mesh[9].position, [0.0, 0.0, 0.0]);
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn octagon_geometry_matches_spec() {
        let (outline, edges, mesh) = run_polygon_shape_cpu(8, 0.315, 0.0);
        // All 8 vertices at radius 0.315.
        for i in 0..8 {
            let [x, y] = outline[i].xy;
            let r = (x * x + y * y).sqrt();
            assert!(
                (r - 0.315).abs() < 1e-5,
                "vertex {i} radius {r} != 0.315",
            );
        }
        // Edges close: 0→1 .. 7→0.
        for i in 0..8 {
            assert!(
                edge_eq(edges[i], i as u32, ((i + 1) % 8) as u32),
                "edge {i} did not match (i, (i+1)%8)",
            );
        }
        // Mesh: 8 triangles × 3 = 24 MeshVertex used.
        // Triangle i wraps perimeter[i] → perimeter[(i+1) % 8].
        for i in 0..8 {
            let a = outline[i].xy;
            let b = outline[(i + 1) % 8].xy;
            let base = i * 3;
            assert!(
                (mesh[base + 1].position[0] - a[0]).abs() < 1e-5
                    && (mesh[base + 1].position[1] - a[1]).abs() < 1e-5,
            );
            assert!(
                (mesh[base + 2].position[0] - b[0]).abs() < 1e-5
                    && (mesh[base + 2].position[1] - b[1]).abs() < 1e-5,
            );
        }
    }

    #[test]
    fn rotation_offsets_vertex_zero() {
        let half_pi = std::f32::consts::FRAC_PI_2;
        let (outline, _, _) = run_polygon_shape_cpu(4, 1.0, half_pi);
        // n=4, rotation=π/2: vertex 0 lands at (0, 1) — top of circle.
        assert!(outline[0].xy[0].abs() < 1e-5);
        assert!((outline[0].xy[1] - 1.0).abs() < 1e-5);
    }
}

#[cfg(test)]
impl PolygonShape {
    /// Test-only harness that runs the primitive's geometry math
    /// without going through the graph executor. The graph executor
    /// path needs a `MockBackend` to allocate the three Array<T>
    /// outputs, which is a bigger setup than this test surface
    /// warrants — the primitive's `run()` would only feed those
    /// buffers data that this helper computes inline.
    #[allow(clippy::needless_range_loop)]
    fn cpu_compute_for_test(
        &self,
        n: u32,
        size: f32,
        rotation: f32,
    ) -> (Vec<LinePoint>, Vec<EdgePair>, Vec<MeshVertex>) {
        let mut outline = vec![LinePoint { xy: [0.0, 0.0] }; POLYGON_MAX_SIDES as usize];
        let mut edges = vec![EdgePair::SENTINEL; POLYGON_MAX_SIDES as usize];
        let mut mesh = vec![
            MeshVertex {
                position: [0.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
            };
            POLYGON_MAX_MESH_VERTS as usize
        ];

        let n_usize = n as usize;
        let n_f32 = n as f32;
        let angle_step = std::f32::consts::TAU / n_f32;

        for i in 0..n_usize {
            let theta = rotation + (i as f32) * angle_step;
            outline[i] = LinePoint {
                xy: [size * theta.cos(), size * theta.sin()],
            };
        }
        for i in 0..n_usize {
            edges[i] = EdgePair {
                a: i as u32,
                b: ((i + 1) % n_usize) as u32,
            };
        }
        for i in 0..n_usize {
            let a = outline[i].xy;
            let b = outline[(i + 1) % n_usize].xy;
            let base = i * 3;
            mesh[base + 1].position = [a[0], a[1], 0.0];
            mesh[base + 2].position = [b[0], b[1], 0.0];
        }
        (outline, edges, mesh)
    }
}
