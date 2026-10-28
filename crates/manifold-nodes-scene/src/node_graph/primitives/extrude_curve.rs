//! `node.extrude_curve` — extrude a 2D outline curve along +Z into a 3D
//! positions+uv grid.
//!
//! `outline: Array<CurvePoint>` sweeps into a `(steps+1) × cols` grid
//! (`cols = outline_len`, or `outline_len + 1` when `close` duplicates the
//! first outline point as the last column — a closed loop): `pos(i,j) =
//! (x_j, y_j, depth × i/steps)`, centred on the plane when `symmetric`.
//! `caps` adds a cap at each end (Blender's Fill Caps). The grid carries
//! exact normals facing out of a closed outline (its columns run backwards
//! when read forwards would face in); wire `node.make_triangles` with Keep
//! Normals downstream.
//! The CPU mirror (`curve_sweep::extrude_grid`) is what a collision shape
//! reads.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::{CurvePoint, MeshVertex};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`depth`
/// f32, `steps` Int→i32, `close`, `symmetric` and `caps` Bool→u32), then the
/// derived `outline_len` (u32), then the codegen-injected `dispatch_count`,
/// padded to a 16-byte multiple. 7 words + 1 pad = 32 bytes. Matches
/// `standalone_for_spec::<ExtrudeCurve>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExtrudeCurveUniforms {
    depth: f32,
    steps: i32,
    close: u32,
    symmetric: u32,
    caps: u32,
    outline_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: ExtrudeCurve,
    type_id: "node.extrude_curve",
    purpose: "Extrude a 2D outline curve (Array<CurvePoint>) along +Z into a (steps+1) x cols positions+uv grid, cols = the outline's point count (or +1 when `close` duplicates the first point as the last column for a closed loop). pos(i,j) = (x_j, y_j, depth * i/steps), with the columns read backwards when a closed outline would otherwise face in (counter-clockwise along +Z, clockwise along -Z), so winding and normals face out of the solid. `symmetric` centres it on the curve plane (z from -depth/2 to depth/2, Blender's Extrude). `caps` adds a start cap (centre, rim) and an end cap (rim, centre) row pair, each fanned from the curve's origin with a hard rim edge, for an outline drawn round its origin. Normals are exact (across the outline on the side, along Z on a cap) — wire node.make_triangles with keep_normals on downstream (src_cols=cols, src_rows=steps+1, +4 with caps).",
    inputs: {
        outline: Array(CurvePoint) required,
        depth: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("depth"),
            label: "Depth",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("steps"),
            label: "Steps",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("close"),
            label: "Close",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("symmetric"),
            label: "Symmetric",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("caps"),
            label: "Fill Caps",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Reach for this to turn a flat 2D shape (polygon_shape, generate_range + array_math profile, pack_curve_xy) into an extruded 3D solid — letters, ribbons, beveled panels. `close=true` wraps the outline into a closed loop (duplicates the first outline point as the last column) for a tube-like cross-section; `close=false` leaves an open sheet. Wire node.make_triangles (keep_normals on) downstream with src_cols = outline point count (+1 if close), src_rows = steps+1, plus 4 with caps. `caps` seals a closed outline drawn round its origin (circle, polygon, star) into a solid; a self-crossing outline has no single inside to fill.",
    examples: [],
    picker: { label: "Extrude Curve", category: Atom },
    summary: "Pushes a flat 2D shape straight through space to build a 3D extrusion — like a cookie cutter dragged through dough. Turns outlines into solid ribbons or beveled panels.",
    category: Geometry3D,
    role: Source,
    aliases: ["extrude curve", "extrude", "push shape"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/extrude_curve_body.wgsl"),
    input_access: [BufferGather],
    derived_uniforms: ["outline_len:u32"],
}

impl Primitive for ExtrudeCurve {
    /// Output `out` is a `(steps+1) × cols` grid — computed override (rows ×
    /// cols), like `node.make_triangles`'s.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let outline_len = input_capacities
            .iter()
            .find(|(p, _)| *p == "outline")
            .map(|(_, n)| *n)?;
        let steps = match params.get("steps") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as u32,
            _ => 1,
        };
        let close = matches!(params.get("close"), Some(ParamValue::Bool(true)));
        let caps = matches!(params.get("caps"), Some(ParamValue::Bool(true)));
        let cols = if close { outline_len + 1 } else { outline_len };
        Some(cols.saturating_mul(steps + 1 + if caps { 4 } else { 0 }))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let depth = ctx.scalar_or_param("depth", 1.0);
        let steps = match ctx.params.get("steps") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as i32,
            _ => 1,
        };
        let flag =
            |name: &str| u32::from(matches!(ctx.params.get(name), Some(ParamValue::Bool(true))));
        let (close, symmetric, caps) = (flag("close"), flag("symmetric"), flag("caps"));

        let Some(outline) = ctx.inputs.array("outline") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let curve_size = std::mem::size_of::<CurvePoint>() as u64;
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let outline_len = (outline.size / curve_size) as u32;
        let dst_capacity = (dst.size / vertex_size) as u32;
        if dst_capacity == 0 || outline_len == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = ExtrudeCurveUniforms {
            depth,
            steps,
            close,
            symmetric,
            caps,
            outline_len,
            dispatch_count: dst_capacity,
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
                    buffer: outline,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [dst_capacity.div_ceil(256), 1, 1],
            "node.extrude_curve",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrude_curve_capacity_is_rows_times_cols() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = ExtrudeCurve::new();
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("steps"), ParamValue::Float(3.0));
        params.insert(std::borrow::Cow::Borrowed("close"), ParamValue::Bool(false));
        let inputs = [("outline", 5_u32)];
        // outline_len=5, steps=3 -> rows=4, cols=5 -> 20.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(20),
        );

        params.insert(std::borrow::Cow::Borrowed("close"), ParamValue::Bool(true));
        // close adds a column: cols=6, rows=4 -> 24.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(24),
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Parity is generated-vs-hand (design
    //! decided #10's proof).
    use super::*;

    fn mk_curve(x: f32, y: f32) -> CurvePoint {
        CurvePoint { xy: [x, y] }
    }

    fn generated_wgsl() -> String {
        manifold_node_engine::freeze::codegen::standalone_for_spec::<ExtrudeCurve>()
            .expect("extrude_curve buffer codegen")
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_extrude(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        outline: &[CurvePoint],
        dst_cap: u32,
        depth: f32,
        steps: i32,
        close: bool,
        caps: bool,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "extrude-curve-test",
        );
        let outline_buf = device.create_buffer(
            std::mem::size_of_val(outline) as u64,
            manifold_gpu::BufferUse::Upload,
        );
        unsafe {
            outline_buf.write(0, bytemuck::cast_slice(outline));
        }
        let dst_buf = device.create_buffer(
            dst_cap as u64 * std::mem::size_of::<MeshVertex>() as u64,
            manifold_gpu::BufferUse::Upload,
        );

        let uniforms = ExtrudeCurveUniforms {
            depth,
            steps,
            close: u32::from(close),
            symmetric: 0,
            caps: u32::from(caps),
            outline_len: outline.len() as u32,
            dispatch_count: dst_cap,
            _pad0: 0,
        };

        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &outline_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &dst_buf,
                offset: 0,
            },
        ];
        let mut enc = device.create_encoder("extrude-curve-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [dst_cap.div_ceil(256), 1, 1],
            "extrude-curve-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dst_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, dst_cap as usize) }.to_vec()
    }

    #[test]
    fn no_end_caps_open_solid_row_zero_and_last_match_outline_exactly() {
        let device = manifold_gpu::testkit::test_device();
        let gen_wgsl = generated_wgsl();
        let outline = vec![mk_curve(0.0, 0.0), mk_curve(2.0, 0.0), mk_curve(1.0, 3.0)];
        const STEPS: i32 = 2;
        const DST_CAP: u32 = 9; // rows=3, cols=3, exact

        let out = dispatch_extrude(
            &device, &gen_wgsl, &outline, DST_CAP, 5.0, STEPS, false, false,
        );
        // Deferred #3: no end caps — row 0 is exactly the outline at z=0.
        for (j, pt) in outline.iter().enumerate() {
            let v = out[j];
            assert!((v.position[0] - pt.xy[0]).abs() < 1e-5);
            assert!((v.position[1] - pt.xy[1]).abs() < 1e-5);
            assert!(
                v.position[2].abs() < 1e-5,
                "row 0 must sit at z=0 (open extrusion)"
            );
        }
    }

    /// The kernel reads a counter-clockwise loop backwards exactly as the
    /// CPU mirror does, so the drawn solid faces out (the mirror's
    /// `swept_solids_face_out` proves the winding and volume).
    #[test]
    fn a_counter_clockwise_loop_matches_the_outward_cpu_mirror() {
        let device = manifold_gpu::testkit::test_device();
        let gen_wgsl = generated_wgsl();
        let points: Vec<[f32; 2]> = (0..=12)
            .map(|i| (std::f32::consts::TAU * i as f32 / 12.0).sin_cos())
            .map(|(s, c)| [c, s])
            .collect();
        let outline: Vec<CurvePoint> = points.iter().map(|p| mk_curve(p[0], p[1])).collect();
        const STEPS: i32 = 2;
        for depth in [1.5f32, -1.5] {
            let grid = manifold_node_engine::scene::curve_sweep::extrude_grid(
                &points,
                depth,
                STEPS as u32,
                false,
                false,
                true,
            );
            let cap = (grid.cols * grid.rows) as u32;
            let out =
                dispatch_extrude(&device, &gen_wgsl, &outline, cap, depth, STEPS, false, true);
            for (k, v) in out.iter().enumerate() {
                for axis in 0..3 {
                    assert!(
                        (v.position[axis] - grid.positions[k][axis]).abs() < 1e-5,
                        "depth {depth} vertex {k} position"
                    );
                    assert!(
                        (v.normal[axis] - grid.normals[k][axis]).abs() < 1e-5,
                        "depth {depth} vertex {k} normal"
                    );
                }
            }
            // A side vertex's normal points away from the axis.
            let side = &out[3 * grid.cols];
            assert!(
                side.position[0] * side.normal[0] + side.position[1] * side.normal[1] > 0.0,
                "depth {depth}: the side faces in"
            );
        }
    }
}
