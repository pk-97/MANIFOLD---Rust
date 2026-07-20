//! `node.tube_from_path` — sweep a circular ring around a centerline path
//! into a 3D positions+uv tube grid (MESH_DEFORM_AND_CURVE_GEOMETRY_
//! DESIGN.md D5/D6, §3 curve→mesh table).
//!
//! `path: Array<CurvePoint>` (XZ plane — x = world X, y = world Z) sweeps
//! into a `path_len × (sides+1)` grid. Optional `lift` (+Y per path point)
//! and `radius_scale` (per path point — composable with a ramp for tapered
//! vines) both degrade to their identity value (0.0 / 1.0) past a short or
//! unwired buffer, matching the deformer family's D2 degrade-to-default
//! contract. Frame per path point: tangent from a central finite
//! difference, reference-up = +Y — **documented limit (Deferred #4): this
//! degenerates when the tangent is (near-)parallel to +Y** (a vertical path
//! segment); parallel-transport frames are deferred. Normals are left zero;
//! wire `node.make_triangles` downstream.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`radius`
/// f32, `sides` Int→i32), then the derived `path_len`/`lift_len`/
/// `radius_scale_len` (u32 each), then the codegen-injected
/// `dispatch_count`, padded to a 16-byte multiple. 6 words + 2 pad = 32
/// bytes. Matches `standalone_for_spec::<TubeFromPath>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TubeFromPathUniforms {
    radius: f32,
    sides: i32,
    path_len: u32,
    lift_len: u32,
    radius_scale_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: TubeFromPath,
    type_id: "node.tube_from_path",
    purpose: "Sweep a circular ring around a centerline path (Array<CurvePoint>, XZ plane: x=world X, y=world Z) into a path_len x (sides+1) positions+uv tube grid. Optional `lift` (+Y per path point) and `radius_scale` (per path point, composable with a ramp for tapered vines) degrade to 0.0 / 1.0 past a short or unwired buffer, never to silent zero-radius. Frame per point: tangent from a central finite difference, reference-up=+Y — degenerates when the tangent is (near-)parallel to +Y (a vertical path segment); parallel-transport frames are deferred. Normals are left zero — wire node.make_triangles downstream (src_cols=sides+1, src_rows=path point count).",
    inputs: {
        path: Array(CurvePoint) required,
        lift: Array(f32) optional,
        radius_scale: Array(f32) optional,
        radius: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sides"),
            label: "Sides",
            ty: ParamType::Int,
            default: ParamValue::Float(8.0),
            range: Some((3.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The vine/ribbon/sweep atom — a path built from generate_range + array_math (a circle: cos/sin of an angle ramp) plus a linear `lift` becomes a helix, the archetypal climbing vine. Wire `radius_scale` to a growth-front ramp (generate_range fraction -> array_math ScaleOffset/Clamp01 threshold against a beat-driven phase) so the tube visibly grows from base to tip — the radius collapses to 0 past the front instead of the whole tube popping in at once. Wire node.make_triangles downstream with src_cols = sides+1, src_rows = the path's point count. The +Y reference frame degenerates on a near-vertical path segment (Deferred #4) — keep paths mostly horizontal/spiraling in v1, or accept the pinch.",
    examples: ["Vine"],
    picker: { label: "Tube From Path", category: Atom },
    summary: "Sweeps a tube of adjustable thickness along a path — the way you'd build a vine, cable, or ribbon from a center-line curve. Thickness and lift can vary per point for tapered, climbing shapes.",
    category: Geometry3D,
    role: Source,
    aliases: ["tube from path", "vine", "ribbon", "sweep", "pipe"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/tube_from_path_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    derived_uniforms: ["path_len:u32", "lift_len:u32", "radius_scale_len:u32"],
}

impl Primitive for TubeFromPath {
    /// Output `out` is a `path_len × (sides+1)` grid — computed override
    /// (rows × cols), like `node.make_triangles`'s.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let path_len = input_capacities
            .iter()
            .find(|(p, _)| *p == "path")
            .map(|(_, n)| *n)?;
        let sides = match params.get("sides") {
            Some(ParamValue::Float(n)) => n.round().max(3.0) as u32,
            _ => 8,
        };
        Some(path_len.saturating_mul(sides + 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let radius = ctx.scalar_or_param("radius", 0.1);
        let sides = match ctx.params.get("sides") {
            Some(ParamValue::Float(n)) => n.round().max(3.0) as i32,
            _ => 8,
        };

        let Some(path) = ctx.inputs.array("path") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        // Optional gather inputs: bind whatever is wired; fall back to the
        // required `path` buffer as a non-null dummy binding when unwired
        // (same pattern as twist_mesh's weights fallback) — the derived
        // `_len` = 0 gates every read back to the identity value, so the
        // dummy's contents are never actually consulted.
        let lift_wired = ctx.inputs.array("lift");
        let lift_buf = lift_wired.unwrap_or(path);
        let lift_len = lift_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let radius_scale_wired = ctx.inputs.array("radius_scale");
        let radius_scale_buf = radius_scale_wired.unwrap_or(path);
        let radius_scale_len = radius_scale_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let curve_size = std::mem::size_of::<CurvePoint>() as u64;
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let path_len = (path.size / curve_size) as u32;
        let dst_capacity = (dst.size / vertex_size) as u32;
        if dst_capacity == 0 || path_len == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design decided #10): the runtime kernel is
            // generated from `wgsl_body` so this atom stays pointwise/fusable
            // in the graph compiler. tube_from_path.wgsl is retained only as
            // the gpu_tests parity oracle. Bindings: uniform(0),
            // buf_path(1, gather), buf_lift(2, gather), buf_radius_scale(3,
            // gather), buf_out(4).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.tube_from_path standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.tube_from_path",
            )
        });

        let uniforms = TubeFromPathUniforms {
            radius,
            sides,
            path_len,
            lift_len,
            radius_scale_len,
            dispatch_count: dst_capacity,
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
                    buffer: path,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: lift_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: radius_scale_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [dst_capacity.div_ceil(256), 1, 1],
            "node.tube_from_path",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn tube_from_path_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let curve_layout = ArrayType::of_known::<CurvePoint>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(TubeFromPath::TYPE_ID, "node.tube_from_path");

        let path_port = TubeFromPath::INPUTS.iter().find(|p| p.name == "path").unwrap();
        assert!(path_port.required);
        assert_eq!(path_port.ty, PortType::Array(curve_layout));

        for name in ["lift", "radius_scale"] {
            let port = TubeFromPath::INPUTS.iter().find(|p| p.name == name).unwrap();
            assert!(!port.required, "{name} should be optional");
            assert_eq!(port.ty, PortType::Array(f32_layout));
        }

        let radius_port = TubeFromPath::INPUTS.iter().find(|p| p.name == "radius").unwrap();
        assert!(!radius_port.required, "radius should be optional (port-shadow)");
        assert_eq!(radius_port.ty, PortType::Scalar(ScalarType::F32));

        assert!(
            !TubeFromPath::INPUTS.iter().any(|p| p.name == "sides"),
            "sides is an int — must not be port-shadowed (P3 brief)"
        );

        assert_eq!(TubeFromPath::OUTPUTS.len(), 1);
        assert_eq!(TubeFromPath::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn tube_from_path_capacity_is_rows_times_cols() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = TubeFromPath::new();
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("sides"), ParamValue::Float(6.0));
        let inputs = [("path", 10_u32)];
        // path_len=10, sides=6 -> cols=7 -> 70.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(70),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TubeFromPath::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.tube_from_path");
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<TubeFromPath>()
            .expect("tube_from_path buffer codegen")
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_tube(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        path: &[CurvePoint],
        lift: Option<&[f32]>,
        radius_scale: Option<&[f32]>,
        dst_cap: u32,
        radius: f32,
        sides: i32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "tube-from-path-test",
        );
        let path_buf = device.create_buffer_shared(std::mem::size_of_val(path) as u64);
        unsafe {
            path_buf.write(0, bytemuck::cast_slice(path));
        }

        let (lift_buf, lift_len) = match lift {
            Some(l) => {
                let b = device.create_buffer_shared((l.len() * 4).max(4) as u64);
                unsafe {
                    b.write(0, bytemuck::cast_slice(l));
                }
                (b, l.len() as u32)
            }
            None => (device.create_buffer_shared(4), 0),
        };
        let (rs_buf, rs_len) = match radius_scale {
            Some(r) => {
                let b = device.create_buffer_shared((r.len() * 4).max(4) as u64);
                unsafe {
                    b.write(0, bytemuck::cast_slice(r));
                }
                (b, r.len() as u32)
            }
            None => (device.create_buffer_shared(4), 0),
        };

        let dst_buf = device.create_buffer_shared(dst_cap as u64 * 48);

        let uniforms = TubeFromPathUniforms {
            radius,
            sides,
            path_len: path.len() as u32,
            lift_len,
            radius_scale_len: rs_len,
            dispatch_count: dst_cap,
            _pad0: 0,
            _pad1: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &path_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &lift_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &rs_buf, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &dst_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("tube-from-path-test");
        enc.dispatch_compute(&pipeline, &bindings, [dst_cap.div_ceil(256), 1, 1], "tube-from-path-test");
        enc.commit_and_wait_completed();

        let ptr = dst_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, dst_cap as usize) }.to_vec()
    }


    #[test]
    fn short_lift_and_radius_scale_degrade_to_identity_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let path: Vec<CurvePoint> = (0..8).map(|i| mk_curve(i as f32, 0.0)).collect();
        let lift = [5.0f32, 5.0]; // only first two points lifted
        let rscale = [0.1f32, 0.1]; // only first two points scaled down
        const SIDES: i32 = 4;
        let dst_cap = 8 * 5;

        let out = dispatch_tube(
            &device, &gen_wgsl, &path, Some(&lift), Some(&rscale), dst_cap, 1.0, SIDES,
        );
        // Row 5 (past both short buffers) should have lift=0 (y of the
        // centerline-ish column col=0 should equal path y=0, not 5.0) and
        // radius_scale=1.0 (full-size ring, not shrunk).
        let row = 5usize;
        let cols = (SIDES + 1) as usize;
        let ring0 = out[row * cols]; // col=0 -> theta=0 -> offset along `right`
        // Distance from center in the XZ-ish plane should be close to full
        // radius (1.0), not the shrunk 0.1 the first two points use.
        let center_y = 0.0f32; // lift degrades to 0.0 past the short buffer
        assert!(
            (ring0.position[1] - center_y).abs() < 0.5,
            "row {row} should NOT carry the short buffer's lift=5.0, got y={}",
            ring0.position[1]
        );
    }
}
