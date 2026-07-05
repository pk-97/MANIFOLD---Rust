//! `node.flatten_3d` — project an `Array<MeshVertex>` (3D positions)
//! to an `Array<CurvePoint>` (2D pre-aspect curve space) via either
//! orthographic or perspective projection.
//!
//! **Output is centred at the origin**, matching the convention of
//! every other `Array<CurvePoint>` producer. `node.draw_lines`
//! applies the center offset + aspect correction itself; no
//! producer should pre-shift to (0.5, 0.5).
//!
//! Orthographic mode matches Wireframe's XY-scale projection
//! bit-for-bit. Perspective mode uses the same projection style as
//! the 4D→2D stage in generator_math::project_4d (s = proj_dist /
//! (proj_dist + z)).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const PROJECT_3D_MODES: &[&str] = &["Orthographic", "Perspective"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`mode`
/// Enum → u32, `proj_scale` f32, `proj_dist` f32), then the derived
/// `active_count` (f32), then the codegen-injected `dispatch_count` (= output
/// capacity, the guard), padded to a 16-byte multiple. 5 words + 3 pad = 32 B.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Project3DUniforms {
    mode: u32,
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: Project3D,
    type_id: "node.flatten_3d",
    purpose: "Project an Array<MeshVertex> (3D positions) to an Array<CurvePoint> (2D pre-aspect curve space) with either orthographic or perspective projection. Output is centred at the origin — node.draw_lines applies the center offset itself, so the convention matches every other Array<CurvePoint> producer (generate_lissajous, etc.). For Wireframe-shaped decompositions: polytope_vertices → Rotate3D → Project3D → render_lines.",
    inputs: {
        in: Array(MeshVertex) required,
        // Port-shadows-param: control-rate wires take precedence over
        // the inline `proj_scale` / `proj_dist` param values. Lets
        // outer-card sliders drive the zoom factor via math nodes
        // (e.g. `outer_scale × wireframe_zoom_factor → proj_scale`).
        proj_scale: ScalarF32 optional,
        proj_dist: ScalarF32 optional,
    },
    outputs: {
        out: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: PROJECT_3D_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("proj_scale"),
            label: "Projection Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("proj_dist"),
            label: "Projection Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Orthographic mode matches Wireframe's bit-exact behaviour (PROJ_SCALE = 0.25 by default; scales xy directly, ignores z). Perspective mode applies s = proj_dist / (proj_dist + z) scaling — useful when the upstream geometry has meaningful depth variation. Active count = input buffer's vertex count; output buffer should be at least the same size.",
    examples: [],
    picker: { label: "Flatten 3D → 2D", category: Atom },
    summary: "Flattens a 3D mesh down to 2D points using a camera, so you can draw it as lines. The projection step for wireframe rendering.",
    category: Geometry3D,
    role: Filter,
    aliases: ["project 3d", "flatten", "perspective", "camera projection"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/project_3d_body.wgsl"),
    derived_uniforms: ["active_count"],
}

impl Primitive for Project3D {
    /// Output `out` is sized to match input `in` — one projected
    /// `CurvePoint` per input vertex.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let proj_scale = ctx.scalar_or_param("proj_scale", 0.25);
        let proj_dist = ctx.scalar_or_param("proj_dist", 3.0);

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let point_size = std::mem::size_of::<CurvePoint>() as u64;
        let in_count = (in_buf.size / vertex_size) as u32;
        let out_capacity = (out_buf.size / point_size) as u32;
        let active_count = in_count.min(out_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path, type-changing in/out + derived active_count).
            // project_3d.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flatten_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flatten_3d",
            )
        });

        let uniforms = Project3DUniforms {
            mode,
            proj_scale,
            proj_dist,
            active_count: active_count as f32,
            dispatch_count: out_capacity,
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
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [out_capacity.div_ceil(256), 1, 1],
            "node.flatten_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn project_3d_declares_mesh_in_and_linepoint_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let point_layout = ArrayType::of_known::<CurvePoint>();
        assert_eq!(Project3D::TYPE_ID, "node.flatten_3d");
        assert_eq!(Project3D::INPUTS.len(), 3);
        assert_eq!(Project3D::INPUTS[0].name, "in");
        assert!(Project3D::INPUTS[0].required);
        assert_eq!(Project3D::INPUTS[0].ty, PortType::Array(mesh_layout));
        for (i, name) in ["proj_scale", "proj_dist"].iter().enumerate() {
            assert_eq!(Project3D::INPUTS[i + 1].name, *name);
            assert!(!Project3D::INPUTS[i + 1].required);
            assert_eq!(
                Project3D::INPUTS[i + 1].ty,
                PortType::Scalar(crate::node_graph::ports::ScalarType::F32)
            );
        }
        assert_eq!(Project3D::OUTPUTS.len(), 1);
        assert_eq!(Project3D::OUTPUTS[0].ty, PortType::Array(point_layout));
    }

    #[test]
    fn project_3d_has_mode_scale_dist_params() {
        let names: Vec<&str> = Project3D::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["mode", "proj_scale", "proj_dist"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Project3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.flatten_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain parity oracle (freeze §12) — project_3d had NO GPU test
    //! before the freeze cutover, so this is added with the conversion: the
    //! GENERATED kernel must reproduce the hand `project_3d.wgsl` point-for-point
    //! in BOTH projection modes, including the inactive-slot collapse to origin.
    //! Direct dispatch (uniform(0), verts(1) read, points(2) read_write).
    use super::*;

    fn mesh_vertex(pos: [f32; 3]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal: [0.0; 3], _pad1: 0.0, uv: [0.0; 2], _pad2: [0.0; 2] }
    }

    /// Dispatch over `capacity` slots and read the CurvePoints back. Group count
    /// uses div_ceil(64) so it covers both the hand (64-wide) and generated
    /// (256-wide) kernels — extra threads in the wider kernel are guarded out.
    fn dispatch_project3d(wgsl: &str, verts: &[MeshVertex], capacity: u32, uniform: &[u8]) -> Vec<CurvePoint> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "p3d-oracle");
        let in_buf =
            device.create_buffer_shared(capacity as u64 * std::mem::size_of::<MeshVertex>() as u64);
        let out_buf =
            device.create_buffer_shared(capacity as u64 * std::mem::size_of::<CurvePoint>() as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(verts));
        }
        let mut enc = device.create_encoder("p3d-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(64), 1, 1],
            "p3d-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const CurvePoint, capacity as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_project3d_matches_hand_kernel_both_modes() {
        let verts = [
            mesh_vertex([0.3, 0.7, 0.5]),
            mesh_vertex([-0.2, 0.4, 1.0]),
            mesh_vertex([0.6, -0.5, -0.3]),
            mesh_vertex([0.1, 0.1, 2.0]),
            mesh_vertex([-0.4, -0.4, 0.0]),
        ];
        const CAPACITY: u32 = 16; // active < capacity → exercises inactive collapse
        let active = verts.len() as u32;
        let proj_scale = 0.25f32;
        let proj_dist = 3.0f32;

        for mode in [0u32, 1u32] {
            // Hand layout: active_count(u32), capacity(u32), mode(u32), pad,
            //              proj_scale(f32), proj_dist(f32), pad, pad.
            let mut hand = Vec::new();
            hand.extend_from_slice(&active.to_le_bytes());
            hand.extend_from_slice(&CAPACITY.to_le_bytes());
            hand.extend_from_slice(&mode.to_le_bytes());
            hand.extend_from_slice(&0u32.to_le_bytes());
            hand.extend_from_slice(&proj_scale.to_le_bytes());
            hand.extend_from_slice(&proj_dist.to_le_bytes());
            hand.extend_from_slice(&[0u8; 8]);

            // Generated layout: mode(u32), proj_scale(f32), proj_dist(f32),
            //                   active_count(f32), dispatch_count(u32), 3 pad.
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&mode.to_le_bytes());
            gen_bytes.extend_from_slice(&proj_scale.to_le_bytes());
            gen_bytes.extend_from_slice(&proj_dist.to_le_bytes());
            gen_bytes.extend_from_slice(&(active as f32).to_le_bytes());
            gen_bytes.extend_from_slice(&CAPACITY.to_le_bytes());
            gen_bytes.extend_from_slice(&[0u8; 12]);

            let hand_wgsl = include_str!("shaders/project_3d.wgsl");
            let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Project3D>()
                .expect("project_3d buffer codegen");

            let from_hand = dispatch_project3d(hand_wgsl, &verts, CAPACITY, &hand);
            let from_gen = dispatch_project3d(&gen_wgsl, &verts, CAPACITY, &gen_bytes);

            for i in 0..CAPACITY as usize {
                assert!(
                    (from_hand[i].xy[0] - from_gen[i].xy[0]).abs() < 1e-6
                        && (from_hand[i].xy[1] - from_gen[i].xy[1]).abs() < 1e-6,
                    "mode {mode} slot {i}: hand={:?} gen={:?}",
                    from_hand[i].xy,
                    from_gen[i].xy
                );
            }
            // Inactive slots collapsed to origin in both.
            for pt in from_gen.iter().skip(active as usize) {
                assert_eq!(pt.xy, [0.0, 0.0], "mode {mode}: inactive slot not origin");
            }
        }
    }
}
