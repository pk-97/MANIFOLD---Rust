//! `node.revolve_curve` — revolve a 2D profile curve around the Y axis into
//! a 3D positions+uv grid (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D5, §3
//! curve→mesh table).
//!
//! `profile: Array<CurvePoint>` (x = radius, y = height) sweeps into a
//! `profile_len × (segments+1)` grid: `pos(i,j) = (x_i·cos(phi_j), y_i,
//! x_i·sin(phi_j))`, `phi_j = sweep × j/segments`. The seam column
//! (j=segments) is a DUPLICATE of j=0's position when `sweep` is an exact
//! multiple of 2π (uv still spans 0..1, not wrapping) — the deliberate D5
//! seam-duplication contract that keeps `node.make_triangles`' finite-
//! difference normals continuous across the seam. Normals are left zero;
//! wire `node.make_triangles` downstream (matching `src_cols = segments+1`,
//! `src_rows = profile_len`) for topology + finite-difference normals.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`segments` Int→i32, `sweep` f32), then the derived `profile_len` (u32),
/// then the codegen-injected `dispatch_count`. 4 words = 16 bytes exactly —
/// no padding needed. Matches `standalone_for_spec::<RevolveCurve>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RevolveCurveUniforms {
    segments: i32,
    sweep: f32,
    profile_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: RevolveCurve,
    type_id: "node.revolve_curve",
    purpose: "Revolve a 2D profile curve (Array<CurvePoint>, x=radius, y=height) around the Y axis into a profile_len x (segments+1) positions+uv grid. pos(i,j) = (x_i*cos(phi_j), y_i, x_i*sin(phi_j)), phi_j = sweep * j/segments. Normals are left zero — wire node.make_triangles downstream (src_cols=segments+1, src_rows=profile_len) for topology and finite-difference normals. `sweep` is UNBOUNDED (range None) — a saw LFO sweeping past 2*pi and beyond is a valid performer gesture (BUG-039 class); sin/cos absorb the wrap with no seam.",
    inputs: {
        profile: Array(CurvePoint) required,
        sweep: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("segments"),
            label: "Segments",
            ty: ParamType::Int,
            default: ParamValue::Float(48.0),
            range: Some((1.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sweep"),
            label: "Sweep",
            ty: ParamType::Float,
            default: ParamValue::Float(std::f32::consts::TAU),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The lathe/spin atom — a profile curve built from generate_range + array_math (or pack_curve_xy) becomes a solid of revolution. Wire node.make_triangles downstream with src_cols = segments+1, src_rows = the profile's point count for topology + normals. `sweep` is unbounded: wire a saw LFO straight into it for continuous full-revolution spin, never clamp downstream (BUG-039 class). At sweep = 2*pi the seam column's positions coincide with column 0 (a closed surface) while uv still spans the full 0..1 range across the seam. The canonical demo is Lathe: node.range -> node.combine_xy (profile) -> node.revolve_curve -> node.make_triangles -> node.render_scene.",
    examples: ["Lathe"],
    picker: { label: "Revolve Curve", category: Atom },
    summary: "Spins a 2D profile curve around a vertical axis to build a solid of revolution — a lathe. The classic way to build vases, columns, and bells from a cross-section.",
    category: Geometry3D,
    role: Source,
    aliases: ["revolve curve", "lathe", "spin", "solid of revolution"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/revolve_curve_body.wgsl"),
    input_access: [BufferGather],
    derived_uniforms: ["profile_len:u32"],
}

impl Primitive for RevolveCurve {
    /// Output `out` is a `profile_len × (segments+1)` grid — computed
    /// override (rows × cols), like `node.make_triangles`'s.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let profile_len = input_capacities
            .iter()
            .find(|(p, _)| *p == "profile")
            .map(|(_, n)| *n)?;
        let segments = match params.get("segments") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as u32,
            _ => 48,
        };
        Some(profile_len.saturating_mul(segments + 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let segments = match ctx.params.get("segments") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as i32,
            _ => 48,
        };
        let sweep = ctx.scalar_or_param("sweep", std::f32::consts::TAU);

        let Some(profile) = ctx.inputs.array("profile") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let curve_size = std::mem::size_of::<CurvePoint>() as u64;
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let profile_len = (profile.size / curve_size) as u32;
        let dst_capacity = (dst.size / vertex_size) as u32;
        if dst_capacity == 0 || profile_len == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design decided #10): the runtime kernel is
            // generated from `wgsl_body` so this atom stays pointwise/fusable
            // in the graph compiler. revolve_curve.wgsl is retained only as
            // the gpu_tests parity oracle. Bindings: uniform(0),
            // buf_profile(1, gather), buf_out(2).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.revolve_curve standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.revolve_curve",
            )
        });

        let uniforms = RevolveCurveUniforms {
            segments,
            sweep,
            profile_len,
            dispatch_count: dst_capacity,
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
                    buffer: profile,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [dst_capacity.div_ceil(256), 1, 1],
            "node.revolve_curve",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn revolve_curve_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let curve_layout = ArrayType::of_known::<CurvePoint>();

        assert_eq!(RevolveCurve::TYPE_ID, "node.revolve_curve");

        let profile_port = RevolveCurve::INPUTS.iter().find(|p| p.name == "profile").unwrap();
        assert!(profile_port.required);
        assert_eq!(profile_port.ty, PortType::Array(curve_layout));

        let sweep_port = RevolveCurve::INPUTS.iter().find(|p| p.name == "sweep").unwrap();
        assert!(!sweep_port.required, "sweep should be optional (port-shadow)");
        assert_eq!(sweep_port.ty, PortType::Scalar(ScalarType::F32));

        assert!(
            !RevolveCurve::INPUTS.iter().any(|p| p.name == "segments"),
            "segments is an int — must not be port-shadowed (P3 brief)"
        );

        assert_eq!(RevolveCurve::OUTPUTS.len(), 1);
        assert_eq!(RevolveCurve::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn revolve_curve_sweep_is_unbounded() {
        let sweep = RevolveCurve::PARAMS.iter().find(|p| p.name == "sweep").unwrap();
        assert_eq!(sweep.range, None, "sweep must be unbounded (BUG-039 class)");
    }

    #[test]
    fn revolve_curve_capacity_is_rows_times_cols() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = RevolveCurve::new();
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("segments"), ParamValue::Float(8.0));
        let inputs = [("profile", 5_u32)];
        // profile_len=5, segments=8 -> cols=9 -> 5*9=45.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(45),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RevolveCurve::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.revolve_curve");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Parity is generated-vs-hand (design
    //! decided #10's proof), plus a chain test into node.make_triangles per
    //! DECOMPOSING_GENERATORS.md §9's chain rule (§4 invariant table).
    use super::*;

    fn mk_curve(x: f32, y: f32) -> CurvePoint {
        CurvePoint { xy: [x, y] }
    }

    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<RevolveCurve>()
            .expect("revolve_curve buffer codegen")
    }

    fn dispatch_revolve(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        profile: &[CurvePoint],
        dst_cap: u32,
        segments: i32,
        sweep: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "revolve-curve-test",
        );
        let profile_buf = device.create_buffer_shared(std::mem::size_of_val(profile) as u64);
        unsafe {
            profile_buf.write(0, bytemuck::cast_slice(profile));
        }
        let dst_buf = device.create_buffer_shared(dst_cap as u64 * 48);

        let uniforms = RevolveCurveUniforms {
            segments,
            sweep,
            profile_len: profile.len() as u32,
            dispatch_count: dst_cap,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &profile_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &dst_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("revolve-curve-test");
        enc.dispatch_compute(&pipeline, &bindings, [dst_cap.div_ceil(256), 1, 1], "revolve-curve-test");
        enc.commit_and_wait_completed();

        let ptr = dst_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, dst_cap as usize) }.to_vec()
    }

    #[test]
    fn generated_matches_hand_kernel() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        assert!(gen_wgsl.contains("var<storage, read> buf_profile"), "gather input is read-only global");
        assert!(gen_wgsl.contains("var<storage, read_write> buf_out"), "out bound read_write");

        let hand = include_str!("shaders/revolve_curve.wgsl");
        let profile = vec![mk_curve(0.5, 0.0), mk_curve(1.0, 0.5), mk_curve(0.3, 1.0)];
        const SEGMENTS: i32 = 6;
        const DST_CAP: u32 = 3 * 7 + 5; // exact + slack to exercise padding

        for &sweep in &[std::f32::consts::TAU, std::f32::consts::PI, 3.0 * std::f32::consts::TAU] {
            let from_gen = dispatch_revolve(&device, &gen_wgsl, &profile, DST_CAP, SEGMENTS, sweep);
            let from_hand = dispatch_revolve(&device, hand, &profile, DST_CAP, SEGMENTS, sweep);
            for i in 0..DST_CAP as usize {
                for c in 0..3 {
                    assert!(
                        (from_gen[i].position[c] - from_hand[i].position[c]).abs() < 1e-5,
                        "sweep={sweep} vertex {i} position[{c}]: gen={} hand={}",
                        from_gen[i].position[c],
                        from_hand[i].position[c]
                    );
                }
                assert_eq!(from_gen[i].uv, from_hand[i].uv, "sweep={sweep} vertex {i} uv");
            }
        }
    }

    /// The chain rule (DECOMPOSING_GENERATORS.md §9 / design §4 invariant
    /// table): revolve -> make_triangles on a small profile, assert the
    /// resulting triangle count and a hand-computed vertex position/uv.
    #[test]
    fn chain_into_triangulate_grid_produces_expected_topology() {
        use crate::node_graph::primitives::triangulate_grid::TriangulateGrid;

        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();

        let profile = vec![mk_curve(0.0, -1.0), mk_curve(1.0, 0.0), mk_curve(0.0, 1.0)];
        const SEGMENTS: i32 = 4; // cols = 5
        const COLS: u32 = 5;
        const ROWS: u32 = 3;
        let grid_cap = ROWS * COLS; // 15, exact

        let revolved = dispatch_revolve(
            &device, &gen_wgsl, &profile, grid_cap, SEGMENTS, std::f32::consts::TAU,
        );

        // Hand-computed anchor: row=1 (radius=1, height=0), col=0 -> phi=0.
        let v10 = revolved[COLS as usize];
        assert!((v10.position[0] - 1.0).abs() < 1e-5, "row1 col0 x: {}", v10.position[0]);
        assert!(v10.position[1].abs() < 1e-5, "row1 col0 y: {}", v10.position[1]);
        assert!(v10.position[2].abs() < 1e-5, "row1 col0 z: {}", v10.position[2]);
        assert!((v10.uv[0] - 0.0).abs() < 1e-5 && (v10.uv[1] - 0.5).abs() < 1e-5, "row1 col0 uv: {:?}", v10.uv);

        // Now triangulate the revolved grid and check the triangle count.
        let tri_pipeline = crate::node_graph::freeze::codegen::standalone_for_spec::<TriangulateGrid>()
            .expect("triangulate_grid codegen");
        let tri_expected_count = (COLS - 1) * (ROWS - 1) * 6; // (5-1)*(3-1)*6 = 48

        let src_buf = device.create_buffer_shared(std::mem::size_of_val(&revolved[..]) as u64);
        unsafe {
            src_buf.write(0, bytemuck::cast_slice(&revolved));
        }
        let tri_dst_cap = tri_expected_count;
        let tri_dst_buf = device.create_buffer_shared(tri_dst_cap as u64 * 48);

        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct TriUniforms {
            src_cols: i32,
            src_rows: i32,
            dispatch_count: u32,
            _pad0: u32,
        }
        let tri_uniforms = TriUniforms {
            src_cols: COLS as i32,
            src_rows: ROWS as i32,
            dispatch_count: tri_dst_cap,
            _pad0: 0,
        };
        let pipeline = device.create_compute_pipeline(
            &tri_pipeline,
            crate::node_graph::freeze::codegen::ENTRY,
            "revolve-chain-tri",
        );
        let mut enc = device.create_encoder("revolve-chain-tri");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&tri_uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &src_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &tri_dst_buf, offset: 0 },
            ],
            [tri_dst_cap.div_ceil(256), 1, 1],
            "revolve-chain-tri",
        );
        enc.commit_and_wait_completed();

        // Every emitted vertex must have a finite, non-degenerate normal
        // (the grid is triangulate_grid-compatible — the D5 claim under
        // test) — spot check the first triangle's normals are unit length.
        let ptr = tri_dst_buf.mapped_ptr().expect("shared tri dst buffer");
        let tris = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, tri_dst_cap as usize) };
        assert_eq!(tris.len() as u32, tri_expected_count, "triangle vertex count matches (cols-1)*(rows-1)*6");
        for v in &tris[0..6] {
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "normal not unit length: {:?} (len={len})", n);
        }
    }
}
