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
use crate::node_graph::camera::CameraMode;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const PROJECT_3D_MODES: &[&str] = &["Orthographic", "Perspective"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`mode`
/// Enum → u32, `proj_scale` f32, `proj_dist` f32), then the derived fields —
/// `active_count` (f32, unchanged), then the camera-branch block added for
/// the optional `camera: Camera` port (docs/CAMERA_AND_LENS_DESIGN.md §2 D3,
/// Amendment 2026-07-12): `cam_right`/`cam_up`/`cam_fwd`/`cam_pos` (vec3 each
/// → 3 scalars per codegen.rs:852-862), `proj_f` (= `1/tan(fov_y/2)` at
/// aspect 1), `cam_near` (the wired camera's near-plane cull threshold), and
/// `use_camera` (u32, 1 = wired) — then the codegen-injected `dispatch_count`
/// (= output capacity, the guard), padded to a 16-byte multiple. 3 params + 1
/// (active_count) + 12 (4 vec3s) + 3 (proj_f/cam_near/use_camera) + 1
/// (dispatch_count) = 20 words ≡ 0 mod 4 — no padding needed. 80 B total.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Project3DUniforms {
    mode: u32,
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    proj_f: f32,
    cam_near: f32,
    use_camera: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: Project3D,
    type_id: "node.flatten_3d",
    purpose: "Project an Array<MeshVertex> (3D positions) to an Array<CurvePoint> (2D pre-aspect curve space) with either orthographic or perspective projection. Output is centred at the origin — node.draw_lines applies the center offset itself, so the convention matches every other Array<CurvePoint> producer (generate_lissajous, etc.). For Wireframe-shaped decompositions: polytope_vertices → Rotate3D → Project3D → render_lines. An optional camera: Camera port overrides both legacy modes (port-shadows-param, docs/CAMERA_AND_LENS_DESIGN.md D3): when wired, every point projects through the same right-handed camera convention node.render_scene uses, so the wireframe path agrees pixel-for-pixel with the scene-renderer family. Unwired, the legacy mode/proj_scale/proj_dist math is bit-identical to before — no migration for existing presets.",
    inputs: {
        in: Array(MeshVertex) required,
        // Port-shadows-param: control-rate wires take precedence over
        // the inline `proj_scale` / `proj_dist` param values. Lets
        // outer-card sliders drive the zoom factor via math nodes
        // (e.g. `outer_scale × wireframe_zoom_factor → proj_scale`).
        proj_scale: ScalarF32 optional,
        proj_dist: ScalarF32 optional,
        // Optional Camera port (D3): when wired, overrides BOTH legacy
        // modes with the canonical camera projection so flatten_3d agrees
        // with render_scene pixel-for-pixel. Unwired, the mode/proj_scale/
        // proj_dist math below is untouched.
        camera: Camera optional,
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
    depth_rule: Terminal,
    composition_notes: "Orthographic mode matches Wireframe's bit-exact behaviour (PROJ_SCALE = 0.25 by default; scales xy directly, ignores z). Perspective mode applies s = proj_dist / (proj_dist + z) scaling — useful when the upstream geometry has meaningful depth variation. Active count = input buffer's vertex count; output buffer should be at least the same size.",
    examples: [],
    picker: { label: "Flatten 3D → 2D", category: Atom },
    summary: "Flattens a 3D mesh down to 2D points using a camera, so you can draw it as lines. The projection step for wireframe rendering.",
    category: Geometry3D,
    role: Filter,
    aliases: ["project 3d", "flatten", "perspective", "camera projection"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/project_3d_body.wgsl"),
    derived_uniforms: [
        "active_count",
        "cam_right:vec3",
        "cam_up:vec3",
        "cam_fwd:vec3",
        "cam_pos:vec3",
        "proj_f",
        "cam_near",
        "use_camera:u32",
    ],
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

        // Optional Camera port (D3): resolved CPU-side into the camera's
        // basis vectors + position (the same values that build its view
        // matrix — no per-node projection math is reintroduced in WGSL,
        // the shader just does dot products against an already-orthonormal
        // basis) plus `proj_f = 1/tan(fov_y/2)` (the perspective scale
        // factor at aspect 1 — "aspect 1: pre-aspect by construction",
        // flatten_3d's output is pre-aspect curve space) and `cam_near`
        // (the wired camera's near-plane cull threshold).
        let cam = ctx.inputs.camera("camera");
        let use_camera: u32 = if cam.is_some() { 1 } else { 0 };
        let (cam_right, cam_up, cam_fwd, cam_pos, proj_f, cam_near) = match cam {
            Some(c) => {
                let proj_f = match c.mode {
                    CameraMode::Perspective { fov_y } => 1.0 / (fov_y * 0.5).tan(),
                    // No current camera-source primitive emits Orthographic
                    // (orbit_camera/free_camera/look_at_camera all hardcode
                    // Perspective) — this branch is unreachable today. XY
                    // scale factor kept consistent with `ortho_rh` at
                    // aspect 1 (`2/(right-left)` with `half_width ==
                    // half_height`) so it's not silently wrong if that ever
                    // changes; the missing depth-divide difference vs a
                    // true orthographic projection is a known limitation.
                    CameraMode::Orthographic { half_height } => 1.0 / half_height,
                };
                (c.right, c.up, c.fwd, c.pos, proj_f, c.near)
            }
            None => ([0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], 0.0, 0.0),
        };

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
            // project_3d.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
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
            cam_right_x: cam_right[0],
            cam_right_y: cam_right[1],
            cam_right_z: cam_right[2],
            cam_up_x: cam_up[0],
            cam_up_y: cam_up[1],
            cam_up_z: cam_up[2],
            cam_fwd_x: cam_fwd[0],
            cam_fwd_y: cam_fwd[1],
            cam_fwd_z: cam_fwd[2],
            cam_pos_x: cam_pos[0],
            cam_pos_y: cam_pos[1],
            cam_pos_z: cam_pos[2],
            proj_f,
            cam_near,
            use_camera,
            dispatch_count: out_capacity,
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
        assert_eq!(Project3D::INPUTS.len(), 4);
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
        assert_eq!(Project3D::INPUTS[3].name, "camera");
        assert!(!Project3D::INPUTS[3].required);
        assert_eq!(Project3D::INPUTS[3].ty, PortType::Camera);
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

