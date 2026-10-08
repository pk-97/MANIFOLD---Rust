//! `node.projected_grid` — a grid of points on a water plane that covers
//! exactly what the camera sees, from its feet to the horizon, at a constant
//! spacing on screen (Johanson 2004, "Real-time water rendering: introducing
//! the projected grid concept"; docs/OCEAN_SURFACE_DESIGN.md D3, section 3.3).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::camera::{Camera, CameraMode};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Codegen uniform layout: params in PARAMS order, the derived camera
/// fields, then `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    columns: i32,
    rows: i32,
    level: f32,
    max_distance: f32,
    margin: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    width: f32,
    height: f32,
    cam_px: f32,
    cam_py: f32,
    cam_pz: f32,
    fwd_x: f32,
    fwd_y: f32,
    fwd_z: f32,
    right_x: f32,
    right_y: f32,
    right_z: f32,
    up_x: f32,
    up_y: f32,
    up_z: f32,
    tan_x: f32,
    tan_y: f32,
    y_top: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// The per-frame camera fields, as the CPU works them out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GridDerived {
    pub cam: [f32; 3],
    pub fwd: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub tan_x: f32,
    pub tan_y: f32,
    pub y_top: f32,
}

manifold_node_engine::primitive! {
    name: ProjectedGrid,
    type_id: "node.projected_grid",
    purpose: "A columns × rows grid of MeshVertex points on the plane y = level, laid so it covers the camera's view from the bottom of the frame to the horizon at constant screen spacing. Each point is a screen-space sample cast onto the plane; rays that miss it, or meet it past Max Distance, stop at Max Distance. UV is the point's world x/z in metres, the rest position node.ocean_displace samples at. Vertex colour is the water's own colour. Row 0 is the horizon, so node.make_triangles' normals point up.",
    inputs: {
        camera: Camera required,
        level: ScalarF32 optional,
        width: ScalarF32 optional,
        height: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef { name: Cow::Borrowed("columns"), label: "Columns", ty: ParamType::Int, default: ParamValue::Float(640.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(360.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("level"), label: "Water Level", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_distance"), label: "Max Distance", ty: ParamType::Float, default: ParamValue::Float(20000.0), range: Some((10.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("margin"), label: "Margin", ty: ParamType::Float, default: ParamValue::Float(0.25), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("color_r"), label: "Colour R", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("color_g"), label: "Colour G", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("color_b"), label: "Colour B", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("width"), label: "Width", ty: ParamType::Float, default: ParamValue::Float(1920.0), range: Some((1.0, 16384.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("height"), label: "Height", ty: ParamType::Float, default: ParamValue::Float(1080.0), range: Some((1.0, 16384.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the scene's camera and system.generator_input's output_width/output_height so the grid matches the frame's aspect. Feed `vertices` to node.ocean_displace, then node.make_triangles with src_cols/src_rows = Columns/Rows. 640 × 360 is about 3 px between points at 1080p. Margin extends the grid past the frame so waves and chop never pull its edge into view. Needs a perspective camera above the water.",
    examples: ["Ocean"],
    picker: { label: "Projected Grid", category: Atom },
    summary: "Lays a grid on a water level that fills the camera's view out to the horizon, dense near the camera and thin far away, the base surface for an ocean.",
    category: Geometry3D,
    role: Source,
    aliases: ["projected grid", "ocean grid", "water plane", "horizon grid", "infinite plane"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/projected_grid_body.wgsl"),
    derived_uniforms: ["cam_px", "cam_py", "cam_pz", "fwd_x", "fwd_y", "fwd_z", "right_x", "right_y", "right_z", "up_x", "up_y", "up_z", "tan_x", "tan_y", "y_top"],
}

#[allow(clippy::too_many_arguments)]
fn grid_uniforms(columns: u32, rows: u32, level: f32, max_distance: f32, margin: f32, color: [f32; 3], size: [f32; 2], d: &GridDerived, count: u32) -> GridUniforms {
    GridUniforms {
        columns: columns as i32,
        rows: rows as i32,
        level,
        max_distance,
        margin,
        color_r: color[0],
        color_g: color[1],
        color_b: color[2],
        width: size[0],
        height: size[1],
        cam_px: d.cam[0],
        cam_py: d.cam[1],
        cam_pz: d.cam[2],
        fwd_x: d.fwd[0],
        fwd_y: d.fwd[1],
        fwd_z: d.fwd[2],
        right_x: d.right[0],
        right_y: d.right[1],
        right_z: d.right[2],
        up_x: d.up[0],
        up_y: d.up[1],
        up_z: d.up[2],
        tan_x: d.tan_x,
        tan_y: d.tan_y,
        y_top: d.y_top,
        dispatch_count: count,
        _pad0: 0,
        _pad1: 0,
    }
}

/// The screen ray at (sx, sy) in normalised device units.
fn ray(d: &GridDerived, sx: f32, sy: f32) -> [f32; 3] {
    std::array::from_fn(|i| d.fwd[i] + sx * d.tan_x * d.right[i] + sy * d.tan_y * d.up[i])
}

/// Negative where the ray meets the water inside `range`.
fn reach(d: &GridDerived, above: f32, range: f32, sx: f32, sy: f32) -> f32 {
    let r = ray(d, sx, sy);
    r[1] + above / range * (r[0] * r[0] + r[2] * r[2]).sqrt()
}

/// The horizon row at range `range` (section 3.3): the highest screen y,
/// over the frame's left edge, centre and right edge, whose ray still meets
/// the water inside `range`.
fn y_top(d: &GridDerived, above: f32, range: f32, margin: f32) -> f32 {
    let edge = 1.0 + margin;
    if above <= 0.0 {
        return -edge;
    }
    let mut best = -edge;
    for sx in [-edge, 0.0, edge] {
        if reach(d, above, range, sx, edge) < 0.0 {
            return edge;
        }
        if reach(d, above, range, sx, -edge) >= 0.0 {
            continue;
        }
        let (mut lo, mut hi) = (-edge, edge);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if reach(d, above, range, sx, mid) < 0.0 { lo = mid } else { hi = mid }
        }
        best = best.max(lo);
    }
    best
}

/// Camera basis, half-angle tangents and horizon row for one frame. `None`
/// for an orthographic camera.
pub(crate) fn derive(cam: &Camera, aspect: f32, level: f32, range: f32, margin: f32) -> Option<GridDerived> {
    let CameraMode::Perspective { fov_y } = cam.mode else { return None };
    let tan_y = (0.5 * fov_y).tan();
    let mut d = GridDerived { cam: cam.pos, fwd: cam.fwd, right: cam.right, up: cam.up, tan_x: tan_y * aspect, tan_y, y_top: 0.0 };
    d.y_top = y_top(&d, cam.pos[1] - level, range, margin);
    Some(d)
}

fn param(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    }
}

impl Primitive for ProjectedGrid {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let get = |name: &str, default: f32| match params.get(name) {
            Some(ParamValue::Float(v)) => v.round().max(2.0) as u32,
            _ => default as u32,
        };
        (port == "vertices").then(|| get("columns", 640.0) * get("rows", 360.0))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let columns = param(ctx, "columns", 640.0).round().max(2.0) as u32;
        let rows = param(ctx, "rows", 360.0).round().max(2.0) as u32;
        let level = ctx.scalar_or_param("level", 0.0);
        let max_distance = param(ctx, "max_distance", 20000.0).max(1.0);
        let margin = param(ctx, "margin", 0.25).max(0.0);
        let (width, height) = (ctx.scalar_or_param("width", 1920.0), ctx.scalar_or_param("height", 1080.0));
        let Some(derived) = derive(&cam, width / height.max(1.0), level, max_distance, margin) else {
            ctx.error("Projected Grid: needs a perspective camera");
            return;
        };
        let Some(out) = ctx.outputs.array("vertices") else {
            return;
        };
        let count = manifold_node_engine::primitives::standalone_pipeline::active_elements::<MeshVertex>(out.size, columns * rows);
        if count == 0 {
            return;
        }
        let color = [param(ctx, "color_r", 1.0), param(ctx, "color_g", 1.0), param(ctx, "color_b", 1.0)];
        let uniforms = grid_uniforms(columns, rows, level, max_distance, margin, color, [width, height], &derived, count);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.projected_grid",
        );
    }
}

/// CPU reference of the WGSL body, for the proofs.
#[cfg(test)]
pub(crate) fn reference_vertex(d: &GridDerived, columns: u32, rows: u32, level: f32, max_distance: f32, margin: f32, idx: u32) -> [f32; 3] {
    let (col, row) = (idx % columns, idx / columns);
    let (u, v) = (col as f32 / (columns - 1) as f32, row as f32 / (rows - 1) as f32);
    let edge = 1.0 + margin;
    let (sx, sy) = ((2.0 * u - 1.0) * edge, d.y_top + (-edge - d.y_top) * v);
    let r = ray(d, sx, sy);
    let above = d.cam[1] - level;
    let horiz = (r[0] * r[0] + r[2] * r[2]).sqrt();
    let dir = if horiz > 1e-6 { [r[0] / horiz, r[2] / horiz] } else { [1.0, 0.0] };
    let mut hit = [d.cam[0] + dir[0] * max_distance, d.cam[2] + dir[1] * max_distance];
    if r[1] < 0.0 && above > 0.0 {
        let t = above / -r[1];
        if t * horiz <= max_distance {
            hit = [d.cam[0] + r[0] * t, d.cam[2] + r[2] * t];
        }
    }
    [hit[0], level, hit[1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(pos: [f32; 3], yaw: f32, pitch: f32, roll: f32) -> Camera {
        let fwd = [pitch.cos() * yaw.sin(), pitch.sin(), -pitch.cos() * yaw.cos()];
        let flat_right = [yaw.cos(), 0.0, yaw.sin()];
        let up0 = cross(flat_right, fwd);
        let right: [f32; 3] = std::array::from_fn(|i| flat_right[i] * roll.cos() + up0[i] * roll.sin());
        let up = cross(right, fwd);
        let mut cam = Camera::default_perspective();
        cam.pos = pos;
        cam.fwd = fwd;
        cam.right = right;
        cam.up = up;
        cam.mode = CameraMode::Perspective { fov_y: 0.6 };
        cam
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }

    fn horizontal(cam: [f32; 3], p: [f32; 3]) -> f32 {
        ((p[0] - cam[0]).powi(2) + (p[2] - cam[2]).powi(2)).sqrt()
    }

    /// Invariant 6 (docs/OCEAN_SURFACE_DESIGN.md section 4): level and
    /// rolled views put the top row's edge vertices on the far circle,
    /// looking down fills the frame with water, looking up collapses.
    #[test]
    fn projected_grid_horizon_cases() {
        let (cols, rows, range, margin) = (64, 32, 20000.0, 0.25);
        let top_edges = |cam: &Camera| {
            let d = derive(cam, 16.0 / 9.0, 0.0, range, margin).unwrap();
            let left = reference_vertex(&d, cols, rows, 0.0, range, margin, 0);
            let right = reference_vertex(&d, cols, rows, 0.0, range, margin, cols - 1);
            (d, horizontal(cam.pos, left), horizontal(cam.pos, right))
        };
        for roll in [0.0f32, 20f32.to_radians()] {
            let cam = camera([0.0, 3.0, 0.0], 0.4, -0.05, roll);
            let (d, left, right) = top_edges(&cam);
            assert!(d.y_top < 1.25, "roll {roll}: horizon should be inside the frame");
            let far = left.max(right);
            assert!((far - range).abs() <= 0.01 * range, "roll {roll}: top row reaches {left}, {right}");
            let bottom = reference_vertex(&d, cols, rows, 0.0, range, margin, (rows - 1) * cols + cols / 2);
            assert!(horizontal(cam.pos, bottom) < 50.0, "roll {roll}: bottom row starts near the camera");
        }
        let down = camera([0.0, 30.0, 0.0], 0.0, -1.2, 0.0);
        assert_eq!(top_edges(&down).0.y_top, 1.25, "looking down: whole frame is water");
        let up = camera([0.0, 3.0, 0.0], 0.0, 1.2, 0.0);
        assert_eq!(top_edges(&up).0.y_top, -1.25, "looking up: grid collapses");
        let under = camera([0.0, -1.0, 0.0], 0.0, -0.3, 0.0);
        assert_eq!(top_edges(&under).0.y_top, -1.25, "below the water: grid collapses");
    }

    /// Row 0 is farther than the last row, so make_triangles' normals face up.
    #[test]
    fn rows_run_from_horizon_to_camera() {
        let cam = camera([5.0, 4.0, -2.0], 1.0, -0.15, 0.0);
        let d = derive(&cam, 16.0 / 9.0, 0.0, 20000.0, 0.25).unwrap();
        let (cols, rows) = (16, 16);
        for col in [0, 7, 15] {
            let far = reference_vertex(&d, cols, rows, 0.0, 20000.0, 0.25, col);
            let near = reference_vertex(&d, cols, rows, 0.0, 20000.0, 0.25, (rows - 1) * cols + col);
            assert!(horizontal(cam.pos, far) > horizontal(cam.pos, near));
        }
        // dy (row +1) × dx (col +1) points up, as tg_compute_normal takes it.
        let p = |i: u32| reference_vertex(&d, cols, rows, 0.0, 20000.0, 0.25, i);
        let (a, b, c) = (p(8 * cols + 8), p(8 * cols + 9), p(9 * cols + 8));
        let dx = [b[0] - a[0], 0.0, b[2] - a[2]];
        let dy = [c[0] - a[0], 0.0, c[2] - a[2]];
        assert!(cross(dy, dx)[1] > 0.0);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::tests_support::*;
    use super::*;

    /// The generated kernel matches the CPU reference vertex for vertex.
    #[test]
    fn projected_grid_matches_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<ProjectedGrid>().expect("projected_grid codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "projected-grid-test");
        let (cols, rows, level, range, margin) = (48u32, 27u32, 0.5f32, 20000.0f32, 0.25f32);
        let cam = test_camera();
        let derived = derive(&cam, 16.0 / 9.0, level, range, margin).unwrap();
        let count = cols * rows;
        let out = device.create_buffer_shared(u64::from(count) * std::mem::size_of::<MeshVertex>() as u64);
        let uniforms = grid_uniforms(cols, rows, level, range, margin, [0.1, 0.2, 0.3], [1920.0, 1080.0], &derived, count);
        let mut enc = device.create_encoder("projected-grid-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "projected-grid-test",
        );
        enc.commit_and_wait_completed();
        let ptr = out.mapped_ptr().expect("shared buffer");
        let got = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, count as usize) };
        for (i, v) in got.iter().enumerate() {
            let want = reference_vertex(&derived, cols, rows, level, range, margin, i as u32);
            let dist = ((want[0] - derived.cam[0]).powi(2) + (want[2] - derived.cam[2]).powi(2)).sqrt();
            let tol = 1e-4 * dist.max(1.0);
            for (a, (have, want)) in v.position.iter().zip(want).enumerate() {
                assert!((have - want).abs() <= tol, "vertex {i} axis {a}: {have} vs {want}");
            }
            assert_eq!(v.uv, [v.position[0], v.position[2]], "vertex {i}: uv is the rest xz");
            assert_eq!(v.color, [0.1, 0.2, 0.3, 1.0]);
            assert_eq!(v.normal, [0.0, 1.0, 0.0]);
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests_support {
    use super::*;

    pub fn test_camera() -> Camera {
        let mut cam = Camera::default_perspective();
        let (yaw, pitch) = (0.3f32, -0.08f32);
        cam.pos = [2.0, 3.5, 1.0];
        cam.fwd = [pitch.cos() * yaw.sin(), pitch.sin(), -pitch.cos() * yaw.cos()];
        cam.right = [yaw.cos(), 0.0, yaw.sin()];
        let (f, r) = (cam.fwd, cam.right);
        cam.up = [r[1] * f[2] - r[2] * f[1], r[2] * f[0] - r[0] * f[2], r[0] * f[1] - r[1] * f[0]];
        cam.mode = CameraMode::Perspective { fov_y: 0.55 };
        cam
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
