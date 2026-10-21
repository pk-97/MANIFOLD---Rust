//! Editor-only viewport overlays (`docs/REALTIME_3D_DESIGN.md` D7, P5):
//! light billboards, the wired show-camera's frustum, and a ground grid.
//!
//! These are drawn ENTIRELY on the CPU, straight onto the tonemapped RGBA8
//! readback pixels the viewport already produces — no new GPU pipeline, no
//! new WGSL, no shader work at all. That is a deliberate scope choice, not a
//! shortcut: the overlay is 2D editor chrome (the same category as a
//! selection outline or a gizmo icon in any DCC tool), not scene geometry —
//! it never enters a GPU pass, so it structurally cannot affect a shadow map,
//! a fusion boundary, or anything the codegen-path / no-monolith rules
//! govern. It rides [`crate::node_graph::camera::Camera::project_to_pixel`],
//! the SAME CPU projection oracle the camera/lens conformance suite already
//! trusts, so overlay placement and the scene's own on-GPU projection can
//! never silently disagree.
//!
//! World-space overlay geometry is built once per frame from the scene's
//! decoded lights/camera (cheap: a handful of line segments), then projected
//! through the EDITOR camera (not the wired show camera — the overlay must
//! move with the navigation camera) and rasterized with a plain
//! integer-stepped line draw.

use manifold_node_engine::scene::camera::Camera;
use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily, PhysicsGizmoShape};

/// One overlay line segment in world space, with its RGBA8 color.
#[derive(Debug, Clone, Copy)]
pub struct WorldLine {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub color: [u8; 4],
}

/// Which overlay layers to draw. All on by default — D7: "light billboards,
/// camera frustum lines, ground grid drawn as editor-only overlay" names all
/// three for Tier 1.
#[derive(Debug, Clone, Copy)]
pub struct ViewportOverlayConfig {
    pub grid: bool,
    pub camera_frustum: bool,
    pub light_billboards: bool,
    /// Ground-plane grid extent (world units, both directions from origin).
    pub grid_extent: f32,
    /// Spacing between grid lines (world units).
    pub grid_step: f32,
    /// World-space half-size of each light billboard's cross.
    pub light_billboard_size: f32,
    /// How far along the wired camera's `fwd` the drawn frustum extends.
    pub frustum_far_visual: f32,
}

impl Default for ViewportOverlayConfig {
    fn default() -> Self {
        Self {
            grid: true,
            camera_frustum: true,
            light_billboards: true,
            grid_extent: 10.0,
            grid_step: 1.0,
            light_billboard_size: 0.25,
            frustum_far_visual: 3.0,
        }
    }
}

const GRID_COLOR: [u8; 4] = [90, 90, 90, 255];
const GRID_AXIS_X_COLOR: [u8; 4] = [180, 70, 70, 255];
const GRID_AXIS_Z_COLOR: [u8; 4] = [70, 90, 180, 255];
const FRUSTUM_COLOR: [u8; 4] = [230, 200, 60, 255];
const LIGHT_COLOR: [u8; 4] = [255, 225, 140, 255];
/// A liquid domain's box.
pub const DOMAIN_BOX_COLOR: [u8; 4] = FRUSTUM_COLOR;

/// The part of the segment `a`..`b` in front of `camera`'s near plane, or
/// `None` when it lies wholly behind. A line passing beside and behind the
/// camera keeps its far end exactly where an unclipped point projects.
pub fn clip_to_near(camera: &Camera, a: [f32; 3], b: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
    let depth = |p: [f32; 3]| (0..3).map(|i| (p[i] - camera.pos[i]) * camera.fwd[i]).sum::<f32>();
    let toward = |from: [f32; 3], to: [f32; 3], t: f32| [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t);
    let near = camera.near.max(1e-4);
    let (da, db) = (depth(a), depth(b));
    match (da < near, db < near) {
        (true, true) => None,
        (true, false) => Some((toward(a, b, (near - da) / (db - da)), b)),
        (false, true) => Some((a, toward(b, a, (near - db) / (da - db)))),
        (false, false) => Some((a, b)),
    }
}

/// Wireframe bounds for a fluid domain, in `color`. The layout is already
/// in scene world-space, so these are the exact translated rectangular
/// bounds; no scene-object transform is applied.
pub fn fluid_domain_lines(layout: FluidDomainLayout, color: [u8; 4]) -> [WorldLine; 12] {
    let min = layout.min;
    let max = [
        min[0] + layout.size[0],
        min[1] + layout.size[1],
        min[2] + layout.size[2],
    ];
    box_lines(min, max, color)
}

/// The twelve edges of the world box `min`..`max`, in `color`.
pub fn box_lines(min: [f32; 3], max: [f32; 3], color: [u8; 4]) -> [WorldLine; 12] {
    let corners = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], max[2]],
        [min[0], max[1], max[2]],
    ];
    let edge = |a: usize, b: usize| WorldLine { a: corners[a], b: corners[b], color };
    [
        edge(0, 1), edge(1, 2), edge(2, 3), edge(3, 0),
        edge(4, 5), edge(5, 6), edge(6, 7), edge(7, 4),
        edge(0, 4), edge(1, 5), edge(2, 6), edge(3, 7),
    ]
}

/// Most interior cell lines drawn along one axis. A cell grid on three box
/// faces then never exceeds `2 * 3 * LATTICE_LINES_PER_AXIS` lines however
/// fine the domain's resolution (Blender and Houdini draw the cell grid on
/// the box faces, not through the volume).
pub const LATTICE_LINES_PER_AXIS: u32 = 256;

/// Upper bound on the lines [`fluid_lattice_lines`] appends.
pub const LATTICE_LINE_BUDGET: usize = 6 * LATTICE_LINES_PER_AXIS as usize;

/// The least on-screen gap, in points, the grid keeps between neighbouring
/// lines. Finer cells are thinned by a whole-cell stride so a dense domain
/// reads as a grid, never a moire or a solid wash.
pub const LATTICE_MIN_POINTS: f32 = 5.0;

const LATTICE_COLOR: [u8; 4] = [230, 200, 60, 70];

/// Which face of the box the grid draws on, per axis. Outside the box's slab
/// on an axis, the face away from the eye (a backdrop that never fills the
/// view); inside it, the wall the camera looks toward.
pub fn lattice_faces(layout: &FluidDomainLayout, eye: [f32; 3], fwd: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|k| {
        let (lo, hi) = (layout.min[k], layout.min[k] + layout.size[k]);
        let toward_max = if eye[k] > lo && eye[k] < hi { fwd[k] >= 0.0 } else { eye[k] < lo + layout.size[k] * 0.5 };
        if toward_max { hi } else { lo }
    })
}

/// The solver's cell grid on the three faces [`lattice_faces`] picks, on the
/// layout's own cell boundaries (`min + i * cell_size`). Each axis draws
/// every `stride`th boundary, the stride set by [`LATTICE_LINES_PER_AXIS`]
/// and by `screen_points(a, b)`, the on-screen distance in points between two
/// world points (read across one cell at the face corner farthest from the
/// eye, where foreshortening is worst), so neighbours stay
/// [`LATTICE_MIN_POINTS`] apart. A floor face resting on
/// the ground plane (y = 0) is skipped: it would sit on the ground grid.
/// The box edges themselves are [`fluid_domain_lines`].
pub fn fluid_lattice_lines(
    layout: FluidDomainLayout,
    eye: [f32; 3],
    fwd: [f32; 3],
    screen_points: impl Fn([f32; 3], [f32; 3]) -> Option<f32>,
    out: &mut Vec<WorldLine>,
) {
    let cell = layout.cell_size as f32;
    let face = lattice_faces(&layout, eye, fwd);
    for k in 0..3 {
        if k == 1 && face[1] == layout.min[1] && layout.min[1].abs() <= cell * 0.5 {
            continue;
        }
        // The face corner farthest from the eye: neighbours are closest on
        // screen there, so keeping them apart at it keeps them apart everywhere.
        let far: [f32; 3] = std::array::from_fn(|a| {
            if a == k {
                face[k]
            } else if eye[a] < layout.min[a] + layout.size[a] * 0.5 {
                layout.min[a] + layout.size[a]
            } else {
                layout.min[a]
            }
        });
        let (i, j) = ((k + 1) % 3, (k + 2) % 3);
        for (along, across) in [(i, j), (j, i)] {
            let cap = layout.cells[across].saturating_sub(1).div_ceil(LATTICE_LINES_PER_AXIS);
            let mut next = far;
            next[across] += if far[across] == layout.min[across] { cell } else { -cell };
            let thin = match screen_points(far, next) {
                Some(gap) if gap > 1e-4 => (LATTICE_MIN_POINTS / gap).ceil() as u32,
                _ => 1,
            };
            let stride = cap.max(thin).max(1);
            let mut step = stride;
            while step < layout.cells[across] {
                let mut a = [0.0; 3];
                a[k] = face[k];
                a[across] = layout.min[across] + step as f32 * cell;
                a[along] = layout.min[along];
                let mut b = a;
                b[along] = layout.min[along] + layout.size[along];
                out.push(WorldLine { a, b, color: LATTICE_COLOR });
                step += stride;
            }
        }
    }
}

/// A ground-plane (XZ, y=0) grid centred at the origin. The two lines through
/// the origin are tinted to read as the X/Z axes (Blender/Maya convention),
/// every other line is neutral gray.
pub fn grid_lines(extent: f32, step: f32) -> Vec<WorldLine> {
    let mut lines = Vec::new();
    if step <= 0.0 || extent <= 0.0 {
        return lines;
    }
    let steps = (extent / step).floor() as i32;
    for i in -steps..=steps {
        let p = i as f32 * step;
        let color = if i == 0 { GRID_AXIS_Z_COLOR } else { GRID_COLOR };
        lines.push(WorldLine { a: [p, 0.0, -extent], b: [p, 0.0, extent], color });
        let color = if i == 0 { GRID_AXIS_X_COLOR } else { GRID_COLOR };
        lines.push(WorldLine { a: [-extent, 0.0, p], b: [extent, 0.0, p], color });
    }
    lines
}

/// A small 3-axis cross at a light's world position — visible from any
/// navigation angle (unlike a screen-facing billboard, which would need the
/// EDITOR camera threaded in here; a fixed 3D cross is simpler and reads
/// fine at the size lights are drawn at).
pub fn light_billboard_lines(pos: [f32; 3], size: f32) -> [WorldLine; 3] {
    let [x, y, z] = pos;
    [
        WorldLine { a: [x - size, y, z], b: [x + size, y, z], color: LIGHT_COLOR },
        WorldLine { a: [x, y - size, z], b: [x, y + size, z], color: LIGHT_COLOR },
        WorldLine { a: [x, y, z - size], b: [x, y, z + size], color: LIGHT_COLOR },
    ]
}

/// Segments per ring of a physics sphere gizmo.
const RING_SEGMENTS: usize = 16;

/// The most lines one physics gizmo draws: a sphere's three rings and its
/// centre cross.
pub const PHYSICS_GIZMO_LINES: usize = 3 * RING_SEGMENTS + 3;

/// One color per physics family, apart from the domain box and the lights.
pub fn physics_gizmo_color(family: PhysicsGizmoFamily) -> [u8; 4] {
    match family {
        PhysicsGizmoFamily::Force => [230, 90, 220, 255],
        PhysicsGizmoFamily::Collider => [240, 140, 50, 255],
        PhysicsGizmoFamily::Body => [110, 220, 110, 255],
        PhysicsGizmoFamily::Source => [80, 200, 255, 255],
        PhysicsGizmoFamily::Drain => [235, 70, 70, 255],
        PhysicsGizmoFamily::Fill => [70, 120, 255, 255],
    }
}

/// A physics gizmo's wireframe into `out`; returns how many lines it
/// wrote. A box draws its 12 edges, a sphere three rings around a centre
/// cross, an arrow its shaft and a four-line head.
pub fn physics_gizmo_lines(gizmo: &PhysicsGizmo, out: &mut [WorldLine; PHYSICS_GIZMO_LINES]) -> usize {
    let color = physics_gizmo_color(gizmo.family);
    let line = |a, b| WorldLine { a, b, color };
    let add = |p: [f32; 3], d: [f32; 3], s: f32| -> [f32; 3] { std::array::from_fn(|i| p[i] + d[i] * s) };
    match gizmo.shape {
        PhysicsGizmoShape::Box { center, half, rotation } => {
            let corner = |k: usize| {
                let local = std::array::from_fn(|i| if k >> i & 1 == 1 { half[i] } else { -half[i] });
                add(center, rotate(rotation, local), 1.0)
            };
            // Corners differ in one bit along an edge.
            let mut n = 0;
            for k in 0..8 {
                for axis in 0..3 {
                    if k >> axis & 1 == 0 {
                        out[n] = line(corner(k), corner(k | 1 << axis));
                        n += 1;
                    }
                }
            }
            n
        }
        PhysicsGizmoShape::Sphere { center, radius } => {
            let mut n = 0;
            for (u, v) in [(0, 1), (1, 2), (0, 2)] {
                let point = |k: usize| {
                    let angle = k as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
                    let mut p = center;
                    p[u] += radius * angle.cos();
                    p[v] += radius * angle.sin();
                    p
                };
                for k in 0..RING_SEGMENTS {
                    out[n] = line(point(k), point(k + 1));
                    n += 1;
                }
            }
            let size = (0.15 * radius).min(0.1);
            for axis in 0..3 {
                let mut d = [0.0; 3];
                d[axis] = size;
                out[n] = line(add(center, d, -1.0), add(center, d, 1.0));
                n += 1;
            }
            n
        }
        PhysicsGizmoShape::Arrow { from, to } => {
            let d: [f32; 3] = std::array::from_fn(|i| to[i] - from[i]);
            let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            out[0] = line(from, to);
            if length <= 0.0 {
                return 1;
            }
            let dir = d.map(|c| c / length);
            // Any axis not along the arrow gives two perpendiculars.
            let pick = if dir[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let p = normalize(cross(dir, pick));
            let q = cross(dir, p);
            let base = add(to, dir, -0.2 * length);
            for (k, side) in [p, p.map(|c| -c), q, q.map(|c| -c)].into_iter().enumerate() {
                out[1 + k] = line(to, add(base, side, 0.08 * length));
            }
            5
        }
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|c| c / length)
}

/// `v` turned by the unit quaternion `q` (xyzw).
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let axis = [q[0], q[1], q[2]];
    let uv = cross(axis, v);
    let uuv = cross(axis, uv);
    std::array::from_fn(|i| v[i] + 2.0 * (q[3] * uv[i] + uuv[i]))
}

/// Wireframe frustum for `cam`, drawn from its position out to
/// `far_visual` world units (NOT the camera's real `far` — that's usually
/// far larger than useful to look at). Four edges from the apex to the far
/// plane corners, plus the far-plane rectangle itself.
pub fn camera_frustum_lines(cam: &Camera, aspect: f32, far_visual: f32) -> Vec<WorldLine> {
    let half_h = match cam.mode {
        manifold_node_engine::scene::camera::CameraMode::Perspective { fov_y } => {
            (fov_y * 0.5).tan() * far_visual
        }
        manifold_node_engine::scene::camera::CameraMode::Orthographic { half_height } => half_height,
    };
    let half_w = half_h * aspect;
    let center = [
        cam.pos[0] + cam.fwd[0] * far_visual,
        cam.pos[1] + cam.fwd[1] * far_visual,
        cam.pos[2] + cam.fwd[2] * far_visual,
    ];
    let corner = |sx: f32, sy: f32| -> [f32; 3] {
        [
            center[0] + cam.right[0] * half_w * sx + cam.up[0] * half_h * sy,
            center[1] + cam.right[1] * half_w * sx + cam.up[1] * half_h * sy,
            center[2] + cam.right[2] * half_w * sx + cam.up[2] * half_h * sy,
        ]
    };
    let tl = corner(-1.0, 1.0);
    let tr = corner(1.0, 1.0);
    let bl = corner(-1.0, -1.0);
    let br = corner(1.0, -1.0);
    vec![
        WorldLine { a: cam.pos, b: tl, color: FRUSTUM_COLOR },
        WorldLine { a: cam.pos, b: tr, color: FRUSTUM_COLOR },
        WorldLine { a: cam.pos, b: bl, color: FRUSTUM_COLOR },
        WorldLine { a: cam.pos, b: br, color: FRUSTUM_COLOR },
        WorldLine { a: tl, b: tr, color: FRUSTUM_COLOR },
        WorldLine { a: tr, b: br, color: FRUSTUM_COLOR },
        WorldLine { a: br, b: bl, color: FRUSTUM_COLOR },
        WorldLine { a: bl, b: tl, color: FRUSTUM_COLOR },
    ]
}

/// A line already resolved to screen space (both endpoints in front of the
/// projecting camera).
#[derive(Debug, Clone, Copy)]
pub struct ScreenLine {
    pub a: (f32, f32),
    pub b: (f32, f32),
    pub color: [u8; 4],
}

/// Project every world-space overlay line through `editor_cam` into
/// `width`x`height` pixel space. A line with either endpoint behind the near
/// plane is dropped (matches `project_to_pixel`'s documented cull) — an
/// acceptable limitation for editor chrome, unlike scene geometry which
/// clips properly on the GPU.
pub fn project_lines(
    editor_cam: &Camera,
    width: u32,
    height: u32,
    lines: &[WorldLine],
) -> Vec<ScreenLine> {
    lines
        .iter()
        .filter_map(|l| {
            let a = editor_cam.project_to_pixel(l.a, width, height)?;
            let b = editor_cam.project_to_pixel(l.b, width, height)?;
            Some(ScreenLine { a: (a.px, a.py), b: (b.px, b.py), color: l.color })
        })
        .collect()
}

/// Build the full overlay line set (grid + camera frustum + light
/// billboards) per `cfg`, in world space. `show_camera` is the WIRED
/// (show-path) camera the scene actually renders with — drawing its frustum
/// lets the performer see where the show camera is pointed while navigating
/// with the editor camera. `None` skips that layer (no camera wired yet).
pub fn build_overlay_lines(
    cfg: &ViewportOverlayConfig,
    show_camera: Option<(&Camera, f32)>,
    light_positions: &[[f32; 3]],
) -> Vec<WorldLine> {
    let mut lines = Vec::new();
    if cfg.grid {
        lines.extend(grid_lines(cfg.grid_extent, cfg.grid_step));
    }
    if cfg.camera_frustum
        && let Some((cam, aspect)) = show_camera
    {
        lines.extend(camera_frustum_lines(cam, aspect, cfg.frustum_far_visual));
    }
    if cfg.light_billboards {
        for pos in light_positions {
            lines.extend(light_billboard_lines(*pos, cfg.light_billboard_size));
        }
    }
    lines
}

/// Rasterize `lines` directly onto an RGBA8 `pixels` buffer (`w*h*4` bytes) —
/// a plain DDA line draw, alpha-blended over whatever is already there
/// (the tonemapped scene render). Out-of-bounds segments are clipped
/// per-pixel (cheap bounds check, no polygon clipper needed for line draws).
pub fn composite_overlay_lines_rgba8(pixels: &mut [u8], w: u32, h: u32, lines: &[ScreenLine]) {
    for line in lines {
        draw_line_rgba8(pixels, w, h, line.a, line.b, line.color);
    }
}

fn draw_line_rgba8(pixels: &mut [u8], w: u32, h: u32, a: (f32, f32), b: (f32, f32), color: [u8; 4]) {
    let (x0, y0) = a;
    let (x1, y1) = b;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 {
        put_pixel(pixels, w, h, x0, y0, color);
        return;
    }
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        put_pixel(pixels, w, h, x0 + dx * t, y0 + dy * t, color);
    }
}

fn put_pixel(pixels: &mut [u8], w: u32, h: u32, x: f32, y: f32, color: [u8; 4]) {
    if x < 0.0 || y < 0.0 {
        return;
    }
    let (xi, yi) = (x as u32, y as u32);
    if xi >= w || yi >= h {
        return;
    }
    let idx = ((yi * w + xi) * 4) as usize;
    if idx + 4 > pixels.len() {
        return;
    }
    let a = color[3] as f32 / 255.0;
    for c in 0..3 {
        let src = color[c] as f32;
        let dst = pixels[idx + c] as f32;
        pixels[idx + c] = (src * a + dst * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    }
    pixels[idx + 3] = 255;
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::scene::camera::Camera;

    #[test]
    fn grid_lines_are_symmetric_and_include_axes() {
        let lines = grid_lines(4.0, 1.0);
        // 9 lines each direction (i in -4..=4) = 18 total.
        assert_eq!(lines.len(), 18);
        assert!(lines.iter().any(|l| l.color == GRID_AXIS_X_COLOR));
        assert!(lines.iter().any(|l| l.color == GRID_AXIS_Z_COLOR));
    }

    #[test]
    fn grid_lines_empty_for_nonpositive_params() {
        assert!(grid_lines(0.0, 1.0).is_empty());
        assert!(grid_lines(4.0, 0.0).is_empty());
    }

    #[test]
    fn fluid_domain_lines_are_twelve_unique_edges_at_translated_bounds() {
        let lines = fluid_domain_lines(FluidDomainLayout {
            min: [2.0, -3.0, 4.0],
            size: [5.0, 6.0, 7.0],
            cells: [8, 8, 8],
            cell_size: 1.0,
        }, DOMAIN_BOX_COLOR);
        assert_eq!(lines.len(), 12);
        let min = [2.0, -3.0, 4.0];
        let max = [7.0, 3.0, 11.0];
        for line in lines {
            for point in [line.a, line.b] {
                assert!(point.iter().enumerate().all(|(axis, value)| {
                    (*value - min[axis]).abs() < f32::EPSILON
                        || (*value - max[axis]).abs() < f32::EPSILON
                }));
            }
        }
        for (i, first) in lines.iter().enumerate() {
            for second in lines.iter().skip(i + 1) {
                let same = (first.a == second.a && first.b == second.b)
                    || (first.a == second.b && first.b == second.a);
                assert!(!same, "duplicate box edge: {first:?}");
            }
        }
    }

    #[test]
    fn physics_gizmos_draw_their_shapes_in_their_family_color() {
        let blank = WorldLine { a: [0.0; 3], b: [0.0; 3], color: [0; 4] };
        let mut lines = [blank; PHYSICS_GIZMO_LINES];
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5);

        // A box turned a quarter about Y: its x extent runs along world z.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let turned = PhysicsGizmo {
            family: PhysicsGizmoFamily::Collider,
            shape: PhysicsGizmoShape::Box { center: [1.0, 0.0, 0.0], half: [0.5, 0.1, 0.2], rotation: [0.0, s, 0.0, s] },
        };
        let n = physics_gizmo_lines(&turned, &mut lines);
        assert_eq!(n, 12);
        for line in &lines[..n] {
            for p in [line.a, line.b] {
                assert!((p[0] - 1.0).abs() <= 0.2 + 1e-5 && p[1].abs() <= 0.1 + 1e-5 && p[2].abs() <= 0.5 + 1e-5, "{p:?}");
            }
            assert_eq!(line.color, physics_gizmo_color(PhysicsGizmoFamily::Collider));
        }

        let sphere = PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Sphere { center: [0.0, 1.0, 0.0], radius: 2.0 } };
        assert_eq!(physics_gizmo_lines(&sphere, &mut lines), PHYSICS_GIZMO_LINES);
        for line in &lines[..3 * RING_SEGMENTS] {
            let r = ((line.a[0]).powi(2) + (line.a[1] - 1.0).powi(2) + line.a[2].powi(2)).sqrt();
            assert!((r - 2.0).abs() < 1e-4, "ring point at radius {r}");
        }

        let arrow = PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Arrow { from: [0.0; 3], to: [0.0, 0.0, 1.0] } };
        assert_eq!(physics_gizmo_lines(&arrow, &mut lines), 5);
        assert!(close(lines[0].b, [0.0, 0.0, 1.0]));
        assert!(lines[1..5].iter().all(|line| close(line.a, [0.0, 0.0, 1.0]) && (line.b[2] - 0.8).abs() < 1e-5));

        let families = [
            PhysicsGizmoFamily::Force, PhysicsGizmoFamily::Collider, PhysicsGizmoFamily::Body,
            PhysicsGizmoFamily::Source, PhysicsGizmoFamily::Drain, PhysicsGizmoFamily::Fill,
        ];
        let mut colors: Vec<_> = families.map(physics_gizmo_color).into_iter().chain([FRUSTUM_COLOR, LIGHT_COLOR]).collect();
        colors.sort();
        colors.dedup();
        assert_eq!(colors.len(), 8, "every family has its own color");
    }

    #[test]
    fn light_billboard_is_three_segments_through_the_light() {
        let lines = light_billboard_lines([1.0, 2.0, 3.0], 0.5);
        assert_eq!(lines.len(), 3);
        for l in &lines {
            let mid = [
                (l.a[0] + l.b[0]) / 2.0,
                (l.a[1] + l.b[1]) / 2.0,
                (l.a[2] + l.b[2]) / 2.0,
            ];
            assert!((mid[0] - 1.0).abs() < 1e-5);
            assert!((mid[1] - 2.0).abs() < 1e-5);
            assert!((mid[2] - 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn camera_frustum_has_eight_edges() {
        let cam = Camera::look_at([0.0, 5.0, -5.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0);
        let lines = camera_frustum_lines(&cam, 16.0 / 9.0, 3.0);
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn project_lines_drops_behind_camera_segments() {
        let cam = Camera::look_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0);
        // A line entirely behind the camera (positive Z, looking down -Z).
        let behind = WorldLine { a: [0.0, 0.0, 5.0], b: [1.0, 0.0, 5.0], color: [255, 0, 0, 255] };
        let visible = WorldLine { a: [0.0, 0.0, -5.0], b: [1.0, 0.0, -5.0], color: [255, 0, 0, 255] };
        let projected = project_lines(&cam, 100, 100, &[behind, visible]);
        assert_eq!(projected.len(), 1, "only the in-front line should survive projection");
    }

    #[test]
    fn composite_draws_pixels_without_panicking_at_edges() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        let lines = [
            ScreenLine { a: (-2.0, -2.0), b: (10.0, 10.0), color: [255, 0, 0, 255] },
            ScreenLine { a: (1.0, 1.0), b: (2.0, 2.0), color: [0, 255, 0, 255] },
        ];
        composite_overlay_lines_rgba8(&mut pixels, 4, 4, &lines);
        // Something inside bounds got painted.
        assert!(pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn composite_is_a_no_op_when_alpha_zero() {
        let mut pixels = vec![10u8; 4 * 4 * 4];
        let lines = [ScreenLine { a: (0.0, 0.0), b: (3.0, 3.0), color: [255, 0, 0, 0] }];
        composite_overlay_lines_rgba8(&mut pixels, 4, 4, &lines);
        // Alpha 0 should leave rgb untouched (only forces alpha channel to 255).
        for px in pixels.chunks_exact(4) {
            assert_eq!(px[0], 10);
            assert_eq!(px[1], 10);
            assert_eq!(px[2], 10);
        }
    }
}
