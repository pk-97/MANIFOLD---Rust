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

use crate::node_graph::camera::Camera;

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
pub fn light_billboard_lines(pos: [f32; 3], size: f32) -> Vec<WorldLine> {
    let [x, y, z] = pos;
    vec![
        WorldLine { a: [x - size, y, z], b: [x + size, y, z], color: LIGHT_COLOR },
        WorldLine { a: [x, y - size, z], b: [x, y + size, z], color: LIGHT_COLOR },
        WorldLine { a: [x, y, z - size], b: [x, y, z + size], color: LIGHT_COLOR },
    ]
}

/// Wireframe frustum for `cam`, drawn from its position out to
/// `far_visual` world units (NOT the camera's real `far` — that's usually
/// far larger than useful to look at). Four edges from the apex to the far
/// plane corners, plus the far-plane rectangle itself.
pub fn camera_frustum_lines(cam: &Camera, aspect: f32, far_visual: f32) -> Vec<WorldLine> {
    let half_h = match cam.mode {
        crate::node_graph::camera::CameraMode::Perspective { fov_y } => {
            (fov_y * 0.5).tan() * far_visual
        }
        crate::node_graph::camera::CameraMode::Orthographic { half_height } => half_height,
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
    use crate::node_graph::camera::Camera;

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
