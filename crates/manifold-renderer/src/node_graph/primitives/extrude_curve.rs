//! `node.extrude_curve` — extrude a 2D outline curve along +Z into a 3D
//! positions+uv grid (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D5, §3
//! curve→mesh table).
//!
//! `outline: Array<CurvePoint>` sweeps into a `(steps+1) × cols` grid
//! (`cols = outline_len`, or `outline_len + 1` when `close` duplicates the
//! first outline point as the last column — a closed loop): `pos(i,j) =
//! (x_j, y_j, depth × i/steps)`. Normals are left zero; wire
//! `node.make_triangles` downstream. No end caps in v1 (Deferred #3 — the
//! extruded solid is open at both ends).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`depth`
/// f32, `steps` Int→i32, `close` Bool→u32), then the derived `outline_len`
/// (u32), then the codegen-injected `dispatch_count`, padded to a 16-byte
/// multiple. 5 words + 3 pad = 32 bytes. Matches
/// `standalone_for_spec::<ExtrudeCurve>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExtrudeCurveUniforms {
    depth: f32,
    steps: i32,
    close: u32,
    outline_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ExtrudeCurve,
    type_id: "node.extrude_curve",
    purpose: "Extrude a 2D outline curve (Array<CurvePoint>) along +Z into a (steps+1) x cols positions+uv grid, cols = the outline's point count (or +1 when `close` duplicates the first point as the last column for a closed loop). pos(i,j) = (x_j, y_j, depth * i/steps). Normals are left zero — wire node.make_triangles downstream (src_cols=cols, src_rows=steps+1). No end caps in v1 (the extruded solid is open at both ends).",
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
    ],
    depth_rule: Terminal,
    composition_notes: "Reach for this to turn a flat 2D shape (polygon_shape, generate_range + array_math profile, pack_curve_xy) into an extruded 3D solid — letters, ribbons, beveled panels. `close=true` wraps the outline into a closed loop (duplicates the first outline point as the last column) for a tube-like cross-section; `close=false` leaves an open sheet. Wire node.make_triangles downstream with src_cols = outline point count (+1 if close), src_rows = steps+1. No end caps — pair with a separate flat cap mesh if a sealed solid is needed (Deferred #3).",
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
        params: &crate::node_graph::effect_node::ParamValues,
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
        let cols = if close { outline_len + 1 } else { outline_len };
        Some(cols.saturating_mul(steps + 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let depth = ctx.scalar_or_param("depth", 1.0);
        let steps = match ctx.params.get("steps") {
            Some(ParamValue::Float(n)) => n.round().max(1.0) as i32,
            _ => 1,
        };
        let close = matches!(ctx.params.get("close"), Some(ParamValue::Bool(true)));

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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design decided #10): the runtime kernel is
            // generated from `wgsl_body` so this atom stays pointwise/fusable
            // in the graph compiler. extrude_curve.wgsl is retained only as
            // the gpu_tests parity oracle. Bindings: uniform(0),
            // buf_outline(1, gather), buf_out(2).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.extrude_curve standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.extrude_curve",
            )
        });

        let uniforms = ExtrudeCurveUniforms {
            depth,
            steps,
            close: u32::from(close),
            outline_len,
            dispatch_count: dst_capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn extrude_curve_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let curve_layout = ArrayType::of_known::<CurvePoint>();

        assert_eq!(ExtrudeCurve::TYPE_ID, "node.extrude_curve");

        let outline_port = ExtrudeCurve::INPUTS.iter().find(|p| p.name == "outline").unwrap();
        assert!(outline_port.required);
        assert_eq!(outline_port.ty, PortType::Array(curve_layout));

        let depth_port = ExtrudeCurve::INPUTS.iter().find(|p| p.name == "depth").unwrap();
        assert!(!depth_port.required, "depth should be optional (port-shadow)");
        assert_eq!(depth_port.ty, PortType::Scalar(ScalarType::F32));

        for name in ["steps", "close"] {
            assert!(
                !ExtrudeCurve::INPUTS.iter().any(|p| p.name == name),
                "{name} is an int/bool — must not be port-shadowed (P3 brief)"
            );
        }

        assert_eq!(ExtrudeCurve::OUTPUTS.len(), 1);
        assert_eq!(ExtrudeCurve::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn extrude_curve_capacity_is_rows_times_cols() {
        use crate::node_graph::effect_node::ParamValues;
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

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ExtrudeCurve::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.extrude_curve");
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<ExtrudeCurve>()
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
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "extrude-curve-test",
        );
        let outline_buf = device.create_buffer_shared(std::mem::size_of_val(outline) as u64);
        unsafe {
            outline_buf.write(0, bytemuck::cast_slice(outline));
        }
        let dst_buf = device.create_buffer_shared(dst_cap as u64 * 48);

        let uniforms = ExtrudeCurveUniforms {
            depth,
            steps,
            close: u32::from(close),
            outline_len: outline.len() as u32,
            dispatch_count: dst_cap,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &outline_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &dst_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("extrude-curve-test");
        enc.dispatch_compute(&pipeline, &bindings, [dst_cap.div_ceil(256), 1, 1], "extrude-curve-test");
        enc.commit_and_wait_completed();

        let ptr = dst_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, dst_cap as usize) }.to_vec()
    }


    #[test]
    fn no_end_caps_open_solid_row_zero_and_last_match_outline_exactly() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let outline = vec![mk_curve(0.0, 0.0), mk_curve(2.0, 0.0), mk_curve(1.0, 3.0)];
        const STEPS: i32 = 2;
        const DST_CAP: u32 = 9; // rows=3, cols=3, exact

        let out = dispatch_extrude(&device, &gen_wgsl, &outline, DST_CAP, 5.0, STEPS, false);
        // Deferred #3: no end caps — row 0 is exactly the outline at z=0.
        for (j, pt) in outline.iter().enumerate() {
            let v = out[j];
            assert!((v.position[0] - pt.xy[0]).abs() < 1e-5);
            assert!((v.position[1] - pt.xy[1]).abs() < 1e-5);
            assert!(v.position[2].abs() < 1e-5, "row 0 must sit at z=0 (open extrusion)");
        }
    }
}
