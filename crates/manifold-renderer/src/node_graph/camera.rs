//! `Camera` — port-data type carried on [`PortType::Camera`](crate::node_graph::ports::PortType::Camera) wires.
//!
//! One `Camera` source primitive (`node.orbit_camera` today, free-look / Euler
//! variants in the future) emits a fully-populated struct each frame; every 3D
//! consumer primitive (mesh renderers, particle-camera splat) takes it as a
//! single `camera: Camera` input and reads position + basis vectors + view
//! matrix + projection params directly instead of re-deriving them from
//! per-renderer scalar params.
//!
//! The struct is plain CPU data — no GPU resource. Backends carry it through
//! the same `(Slot → value)` map shape that scalars use; the executor drains
//! `pending_camera_writes` after each node's `evaluate` returns, parallel to
//! the scalar drain.
//!
//! Aspect lives on the **consumer**, not the camera, because the consumer
//! knows its render target. The camera struct carries the camera's intrinsic
//! state (position, basis, view matrix, FOV / ortho height, near, far) and
//! exposes helpers (`Camera::proj`, `Camera::view_proj`) that take the
//! consumer-supplied aspect to build the projection.

use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul, perspective_rh};

/// Discriminator for the projection style. Carried in [`Camera::mode`] so
/// consumers that have meaningfully different code paths (e.g. fluid scatter's
/// toroidal-wrap orthographic vs cull-behind-camera perspective) can dispatch
/// explicitly rather than rely on a sentinel-FOV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraMode {
    /// Standard pinhole perspective with vertical field of view in radians.
    Perspective { fov_y: f32 },
    /// Orthographic with half-frustum-height in world units. Width derives
    /// from the consumer's aspect.
    Orthographic { half_height: f32 },
}

/// Camera struct flowing through `PortType::Camera` wires.
///
/// Built once per frame in `node.orbit_camera::run()` (or future camera
/// sources), passed by value to every downstream consumer. ~96 bytes —
/// trivially cheap to clone per wire per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// World-space camera position.
    pub pos: [f32; 3],
    /// Unit vector pointing where the camera is looking.
    pub fwd: [f32; 3],
    /// Unit vector along the camera's right axis.
    pub right: [f32; 3],
    /// Unit vector along the camera's up axis.
    pub up: [f32; 3],
    /// Near plane distance.
    pub near: f32,
    /// Far plane distance.
    pub far: f32,
    /// Projection mode (perspective FOV or ortho half-height).
    pub mode: CameraMode,
    /// Precomputed right-handed view matrix (world → camera-space).
    /// View doesn't depend on aspect, so it's cached here.
    pub view: [[f32; 4]; 4],
}

impl Camera {
    /// Identity-ish default — origin position looking down +Z, FOV 60°, sensible
    /// near/far. Provided so consumers can have a sane fallback when nothing is
    /// wired (though wiring is required in practice).
    pub fn default_perspective() -> Self {
        let pos = [0.0, 0.0, -3.0];
        let target = [0.0, 0.0, 0.0];
        let up = [0.0, 1.0, 0.0];
        let near = 0.05;
        let far = 200.0;
        let fov_y = std::f32::consts::FRAC_PI_3;
        let view = look_at_rh(pos, target, up);
        let fwd = normalize3(sub3(target, pos));
        let right = normalize3(cross3(fwd, up));
        let up_corrected = normalize3(cross3(right, fwd));
        Self {
            pos,
            fwd,
            right,
            up: up_corrected,
            near,
            far,
            mode: CameraMode::Perspective { fov_y },
            view,
        }
    }

    /// Build an orbit-style perspective camera from `(orbit, tilt, distance,
    /// fov_y, look_y, roll)`. The orbit math mirrors the inline formula every 3D
    /// renderer used pre-Camera-port (`render_3d_mesh`, `render_instanced_3d_mesh`,
    /// `digital_plants_render`). `roll` rotates the
    /// `right` and `up` vectors around `fwd` so the camera banks while still
    /// orbiting the same target — radians, positive = clockwise when looking
    /// along `fwd`. `look_at_rh` is rebuilt from the rolled `up` so the view
    /// matrix reflects the roll too.
    pub fn orbit_perspective(
        orbit: f32,
        tilt: f32,
        distance: f32,
        fov_y: f32,
        look_y: f32,
        roll: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let target = [0.0, look_y, 0.0];
        let pos = [
            distance * orbit.cos() * tilt.cos(),
            distance * tilt.sin() + look_y,
            distance * orbit.sin() * tilt.cos(),
        ];
        let world_up = [0.0, 1.0, 0.0];
        let fwd = normalize3(sub3(target, pos));
        let right0 = normalize3(cross3(fwd, world_up));
        let up0 = normalize3(cross3(right0, fwd));
        // Roll around the fwd axis. Rotate (right, up) by `roll`.
        let (s, c) = (roll.sin(), roll.cos());
        let right = normalize3([
            right0[0] * c + up0[0] * s,
            right0[1] * c + up0[1] * s,
            right0[2] * c + up0[2] * s,
        ]);
        let up = normalize3([
            -right0[0] * s + up0[0] * c,
            -right0[1] * s + up0[1] * c,
            -right0[2] * s + up0[2] * c,
        ]);
        let view = look_at_rh(pos, target, up);
        Self {
            pos,
            fwd,
            right,
            up,
            near,
            far,
            mode: CameraMode::Perspective { fov_y },
            view,
        }
    }

    /// Build a free-look perspective camera from world-space `pos` and Euler
    /// angles (radians): `yaw` about world up (Y), `pitch` about the camera's
    /// right axis, `roll` about `fwd`. This is the gizmo- and import-friendly
    /// authoring mode — position + orientation directly, no orbit target.
    ///
    /// `fwd` is derived from yaw/pitch against world +Z (yaw=pitch=roll=0 looks
    /// down `-Z`, matching `look_at_rh`'s eye/target convention where `fwd =
    /// normalize(target - eye)` and `default_perspective`'s eye-at-`-Z`-looking-
    /// at-origin setup). `right`/`up` are derived via cross products with world
    /// up, then `roll` rotates them around `fwd` — bit-for-bit the same
    /// roll-around-fwd rotation `orbit_perspective` uses.
    pub fn from_pos_euler(
        pos: [f32; 3],
        yaw: f32,
        pitch: f32,
        roll: f32,
        fov_y: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let world_up = [0.0, 1.0, 0.0];
        // Yaw rotates -Z around Y; pitch tilts that up/down around the
        // resulting right axis. Standard spherical-to-Cartesian derivation.
        let fwd = normalize3([
            -yaw.sin() * pitch.cos(),
            pitch.sin(),
            -yaw.cos() * pitch.cos(),
        ]);
        let right0 = normalize3(cross3(fwd, world_up));
        let up0 = normalize3(cross3(right0, fwd));
        // Roll around the fwd axis. Rotate (right, up) by `roll` — same
        // formula as `orbit_perspective`.
        let (s, c) = (roll.sin(), roll.cos());
        let right = normalize3([
            right0[0] * c + up0[0] * s,
            right0[1] * c + up0[1] * s,
            right0[2] * c + up0[2] * s,
        ]);
        let up = normalize3([
            -right0[0] * s + up0[0] * c,
            -right0[1] * s + up0[1] * c,
            -right0[2] * s + up0[2] * c,
        ]);
        let view = look_at_rh(pos, [pos[0] + fwd[0], pos[1] + fwd[1], pos[2] + fwd[2]], up);
        Self {
            pos,
            fwd,
            right,
            up,
            near,
            far,
            mode: CameraMode::Perspective { fov_y },
            view,
        }
    }

    /// Build a look-at perspective camera from world-space `pos`/`target` and
    /// an approximate `up` hint (orthonormalized against `fwd`, same as every
    /// other builder in this file). `fwd` points from `pos` toward `target`.
    pub fn look_at(
        pos: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        fov_y: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let fwd = normalize3(sub3(target, pos));
        let right = normalize3(cross3(fwd, up));
        let up_corrected = normalize3(cross3(right, fwd));
        let view = look_at_rh(pos, target, up_corrected);
        Self {
            pos,
            fwd,
            right,
            up: up_corrected,
            near,
            far,
            mode: CameraMode::Perspective { fov_y },
            view,
        }
    }

    /// Projection matrix for the given consumer-supplied aspect ratio
    /// (`width / height` of the consumer's render target). Aspect lives here
    /// rather than on the struct because the camera primitive doesn't know
    /// the consumer's target dims.
    pub fn proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        match self.mode {
            CameraMode::Perspective { fov_y } => {
                perspective_rh(fov_y, aspect, self.near, self.far)
            }
            CameraMode::Orthographic { half_height } => {
                let half_width = half_height * aspect;
                ortho_rh(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    self.near,
                    self.far,
                )
            }
        }
    }

    /// Combined `proj(aspect) * view`. Convenience for shader uniforms that
    /// take a single VP matrix.
    pub fn view_proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        mat4_mul(self.proj(aspect), self.view)
    }

    /// THE reference oracle for every conformance gate in the camera/lens
    /// cluster (`docs/CAMERA_AND_LENS_DESIGN.md` §2 D2) — GPU projection
    /// paths are verified against this function, never against each other
    /// or a screenshot. Projects a world-space point to a pixel coordinate
    /// in a `width x height` render target using this camera's `view_proj`.
    /// Returns `None` when the point is behind the near plane (`clip.w <=
    /// 0`), matching the standard perspective-divide cull.
    pub fn project_to_pixel(&self, world: [f32; 3], width: u32, height: u32) -> Option<PixelProjection> {
        let aspect = width as f32 / (height.max(1) as f32);
        let vp = self.view_proj(aspect);
        let clip = mat4_mul_vec4(vp, [world[0], world[1], world[2], 1.0]);
        if clip[3] <= 0.0 {
            return None;
        }
        let ndc = [clip[0] / clip[3], clip[1] / clip[3]];
        // Metal rasterizes y-down: NDC +y (up) lands at the SMALLER pixel row.
        let px = (ndc[0] * 0.5 + 0.5) * width as f32;
        let py = (1.0 - (ndc[1] * 0.5 + 0.5)) * height as f32;
        let view_z = dot3(sub3(world, self.pos), self.fwd);
        Some(PixelProjection { px, py, ndc, depth: clip[2] / clip[3], view_z })
    }
}

/// Result of [`Camera::project_to_pixel`] — the committed CPU oracle shape
/// (`docs/CAMERA_AND_LENS_DESIGN.md` §2 D2). Every field is derived from the
/// same `view_proj` every GPU consumer is expected to agree with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelProjection {
    /// Pixel x in `[0, width)`.
    pub px: f32,
    /// Pixel y in `[0, height)`, y-down (Metal viewport convention).
    pub py: f32,
    /// Pre-viewport NDC, `+y` up, each component nominally in `[-1, 1]`.
    pub ndc: [f32; 2],
    /// Clip-space depth in `[0, 1]` (raw, non-linear — Metal depth range).
    pub depth: f32,
    /// Linear view-space distance along `-fwd` (i.e. `dot(world - pos,
    /// fwd)`), for CoC / SSAO consumers that want a linear depth.
    pub view_z: f32,
}

/// Multiply a column-major 4x4 matrix (`m[col][row]`, matching `mat4_mul`'s
/// convention) by a column vector.
fn mat4_mul_vec4(m: [[f32; 4]; 4], v: [f32; 4]) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for (row, slot) in out.iter_mut().enumerate() {
        *slot = m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row] * v[3];
    }
    out
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// Right-handed orthographic projection matrix. Mirrors `perspective_rh` in
/// `generators::mesh_pipeline` but for ortho — needed for fluid-scatter's
/// ortho mode, which `mesh_pipeline` doesn't ship.
fn ortho_rh(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
) -> [[f32; 4]; 4] {
    let rml = right - left;
    let tmb = top - bottom;
    let fmn = far - near;
    [
        [2.0 / rml, 0.0, 0.0, 0.0],
        [0.0, 2.0 / tmb, 0.0, 0.0],
        [0.0, 0.0, -1.0 / fmn, 0.0],
        [
            -(right + left) / rml,
            -(top + bottom) / tmb,
            -near / fmn,
            1.0,
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_perspective_matches_legacy_eye_formula() {
        let cam = Camera::orbit_perspective(0.7, 0.3, 4.0, 0.9, 0.0, 0.0, 0.05, 200.0);
        let expected_pos = [
            4.0 * 0.7_f32.cos() * 0.3_f32.cos(),
            4.0 * 0.3_f32.sin(),
            4.0 * 0.7_f32.sin() * 0.3_f32.cos(),
        ];
        for (axis, &expected) in expected_pos.iter().enumerate() {
            assert!(
                (cam.pos[axis] - expected).abs() < 1e-6,
                "axis {axis}: got {} expected {}",
                cam.pos[axis],
                expected,
            );
        }
    }

    #[test]
    fn fwd_right_up_form_orthonormal_basis() {
        let cam = Camera::orbit_perspective(0.5, 0.4, 3.0, 1.0, 0.1, 0.0, 0.1, 100.0);
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!(dot(cam.fwd, cam.fwd).abs() - 1.0 < 1e-5);
        assert!(dot(cam.right, cam.right).abs() - 1.0 < 1e-5);
        assert!(dot(cam.up, cam.up).abs() - 1.0 < 1e-5);
        assert!(dot(cam.fwd, cam.right).abs() < 1e-5);
        assert!(dot(cam.fwd, cam.up).abs() < 1e-5);
        assert!(dot(cam.right, cam.up).abs() < 1e-5);
    }

    #[test]
    fn roll_rotates_right_up_around_fwd_and_preserves_basis() {
        let no_roll = Camera::orbit_perspective(0.5, 0.4, 3.0, 1.0, 0.0, 0.0, 0.1, 100.0);
        let rolled = Camera::orbit_perspective(0.5, 0.4, 3.0, 1.0, 0.0, 1.5, 0.1, 100.0);
        // pos + fwd are roll-invariant
        for axis in 0..3 {
            assert!((no_roll.pos[axis] - rolled.pos[axis]).abs() < 1e-5);
            assert!((no_roll.fwd[axis] - rolled.fwd[axis]).abs() < 1e-5);
        }
        // Rolled basis is still orthonormal
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!((dot(rolled.right, rolled.right) - 1.0).abs() < 1e-5);
        assert!((dot(rolled.up, rolled.up) - 1.0).abs() < 1e-5);
        assert!(dot(rolled.fwd, rolled.right).abs() < 1e-5);
        assert!(dot(rolled.fwd, rolled.up).abs() < 1e-5);
        assert!(dot(rolled.right, rolled.up).abs() < 1e-5);
        // And actually different from the unrolled basis
        let diff = (no_roll.right[0] - rolled.right[0]).abs()
            + (no_roll.right[1] - rolled.right[1]).abs()
            + (no_roll.right[2] - rolled.right[2]).abs();
        assert!(diff > 0.01, "roll should change `right` vector");
    }

    #[test]
    fn default_perspective_populates_all_fields() {
        let cam = Camera::default_perspective();
        assert_eq!(cam.pos, [0.0, 0.0, -3.0]);
        assert!(matches!(cam.mode, CameraMode::Perspective { .. }));
    }

    #[test]
    fn from_pos_euler_zero_angles_looks_down_negative_z() {
        let cam = Camera::from_pos_euler([1.0, 2.0, 3.0], 0.0, 0.0, 0.0, 0.9, 0.05, 200.0);
        assert!((cam.fwd[0] - 0.0).abs() < 1e-6);
        assert!((cam.fwd[1] - 0.0).abs() < 1e-6);
        assert!((cam.fwd[2] - -1.0).abs() < 1e-6, "expected -Z forward, got {:?}", cam.fwd);
        assert_eq!(cam.pos, [1.0, 2.0, 3.0]);
        assert!(matches!(cam.mode, CameraMode::Perspective { fov_y } if (fov_y - 0.9).abs() < 1e-6));
    }

    #[test]
    fn from_pos_euler_basis_is_orthonormal_with_and_without_roll() {
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        for roll in [0.0_f32, 0.4, 1.7] {
            let cam = Camera::from_pos_euler([0.0, 0.0, 0.0], 0.6, 0.3, roll, 1.0, 0.1, 100.0);
            assert!((dot(cam.fwd, cam.fwd) - 1.0).abs() < 1e-5);
            assert!((dot(cam.right, cam.right) - 1.0).abs() < 1e-5);
            assert!((dot(cam.up, cam.up) - 1.0).abs() < 1e-5);
            assert!(dot(cam.fwd, cam.right).abs() < 1e-5);
            assert!(dot(cam.fwd, cam.up).abs() < 1e-5);
            assert!(dot(cam.right, cam.up).abs() < 1e-5);
        }
    }

    #[test]
    fn look_at_fwd_points_from_pos_toward_target() {
        let pos = [2.0, 1.0, 0.0];
        let target = [2.0, 1.0, 5.0];
        let cam = Camera::look_at(pos, target, [0.0, 1.0, 0.0], 0.9, 0.05, 200.0);
        let expected_fwd = normalize3(sub3(target, pos));
        for (axis, &expected) in expected_fwd.iter().enumerate() {
            assert!(
                (cam.fwd[axis] - expected).abs() < 1e-6,
                "axis {axis}: got {} expected {}",
                cam.fwd[axis],
                expected,
            );
        }
        assert_eq!(cam.pos, pos);
        assert!(matches!(cam.mode, CameraMode::Perspective { .. }));
    }

    #[test]
    fn look_at_basis_is_orthonormal() {
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let cam = Camera::look_at([1.0, 2.0, 3.0], [-4.0, 0.5, 2.0], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0);
        assert!((dot(cam.fwd, cam.fwd) - 1.0).abs() < 1e-5);
        assert!((dot(cam.right, cam.right) - 1.0).abs() < 1e-5);
        assert!((dot(cam.up, cam.up) - 1.0).abs() < 1e-5);
        assert!(dot(cam.fwd, cam.right).abs() < 1e-5);
        assert!(dot(cam.fwd, cam.up).abs() < 1e-5);
        assert!(dot(cam.right, cam.up).abs() < 1e-5);
    }

    #[test]
    fn project_to_pixel_center_point_lands_on_center_pixel() {
        // Camera at origin looking down -Z (fov_y irrelevant for an
        // on-axis point — s=0 regardless of the perspective scale factor).
        let cam = Camera::look_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_2, 0.1, 100.0);
        let proj = cam.project_to_pixel([0.0, 0.0, -5.0], 800, 600).expect("in front of camera");
        assert!((proj.ndc[0] - 0.0).abs() < 1e-5, "ndc.x = {}", proj.ndc[0]);
        assert!((proj.ndc[1] - 0.0).abs() < 1e-5, "ndc.y = {}", proj.ndc[1]);
        assert!((proj.px - 400.0).abs() < 1e-3, "px = {}", proj.px);
        assert!((proj.py - 300.0).abs() < 1e-3, "py = {}", proj.py);
    }

    #[test]
    fn project_to_pixel_45deg_fov_point_matches_hand_derivation() {
        // fov_y = 90 deg (half-angle 45 deg, f = 1/tan(45 deg) = 1) so a
        // point at view-space (1, 0, -1) sits exactly on the right frustum
        // edge: clip.x = f/aspect * 1 = 1, clip.w = 1 -> ndc.x = 1.
        let cam = Camera::look_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_2, 0.1, 100.0);
        let proj = cam.project_to_pixel([1.0, 0.0, -1.0], 512, 512).expect("in front of camera");
        assert!((proj.ndc[0] - 1.0).abs() < 1e-5, "ndc.x = {}", proj.ndc[0]);
        assert!((proj.ndc[1] - 0.0).abs() < 1e-5, "ndc.y = {}", proj.ndc[1]);
        assert!((proj.px - 512.0).abs() < 1e-3, "px = {}", proj.px);
        assert!((proj.py - 256.0).abs() < 1e-3, "py = {}", proj.py);
    }

    #[test]
    fn project_to_pixel_behind_camera_is_none() {
        let cam = Camera::look_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_2, 0.1, 100.0);
        // +Z is behind a camera looking down -Z.
        assert_eq!(cam.project_to_pixel([0.0, 0.0, 1.0], 512, 512), None);
    }

    #[test]
    fn proj_dispatches_on_mode() {
        let mut cam = Camera::orbit_perspective(0.0, 0.0, 5.0, 1.0, 0.0, 0.0, 0.1, 100.0);
        let _persp = cam.proj(16.0 / 9.0);
        cam.mode = CameraMode::Orthographic { half_height: 1.0 };
        let ortho = cam.proj(16.0 / 9.0);
        // Ortho-specific signature: [3][3] (perspective stores -1 there for
        // perspective divide, ortho stores 1.0)
        assert!((ortho[2][3] - 0.0).abs() < 1e-5);
    }
}
