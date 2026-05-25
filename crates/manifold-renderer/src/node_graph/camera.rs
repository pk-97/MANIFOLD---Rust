//! `Camera` — port-data type carried on [`PortType::Camera`](crate::node_graph::ports::PortType::Camera) wires.
//!
//! One `Camera` source primitive (`node.camera_orbit` today, free-look / Euler
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
/// Built once per frame in `node.camera_orbit::run()` (or future camera
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
    /// fov_y, look_y)`. The orbit math mirrors the inline formula every 3D
    /// renderer used pre-Camera-port (`render_3d_mesh`, `render_instanced_3d_mesh`,
    /// `render_3d_mesh_pbr_ibl`, `digital_plants_render`).
    pub fn orbit_perspective(
        orbit: f32,
        tilt: f32,
        distance: f32,
        fov_y: f32,
        look_y: f32,
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
        let view = look_at_rh(pos, target, world_up);
        let fwd = normalize3(sub3(target, pos));
        let right = normalize3(cross3(fwd, world_up));
        let up = normalize3(cross3(right, fwd));
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
        let cam = Camera::orbit_perspective(0.7, 0.3, 4.0, 0.9, 0.0, 0.05, 200.0);
        let expected_pos = [
            4.0 * 0.7_f32.cos() * 0.3_f32.cos(),
            4.0 * 0.3_f32.sin(),
            4.0 * 0.7_f32.sin() * 0.3_f32.cos(),
        ];
        for axis in 0..3 {
            assert!(
                (cam.pos[axis] - expected_pos[axis]).abs() < 1e-6,
                "axis {axis}: got {} expected {}",
                cam.pos[axis],
                expected_pos[axis],
            );
        }
    }

    #[test]
    fn fwd_right_up_form_orthonormal_basis() {
        let cam = Camera::orbit_perspective(0.5, 0.4, 3.0, 1.0, 0.1, 0.1, 100.0);
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!(dot(cam.fwd, cam.fwd).abs() - 1.0 < 1e-5);
        assert!(dot(cam.right, cam.right).abs() - 1.0 < 1e-5);
        assert!(dot(cam.up, cam.up).abs() - 1.0 < 1e-5);
        assert!(dot(cam.fwd, cam.right).abs() < 1e-5);
        assert!(dot(cam.fwd, cam.up).abs() < 1e-5);
        assert!(dot(cam.right, cam.up).abs() < 1e-5);
    }

    #[test]
    fn default_perspective_populates_all_fields() {
        let cam = Camera::default_perspective();
        assert_eq!(cam.pos, [0.0, 0.0, -3.0]);
        assert!(matches!(cam.mode, CameraMode::Perspective { .. }));
    }

    #[test]
    fn proj_dispatches_on_mode() {
        let mut cam = Camera::orbit_perspective(0.0, 0.0, 5.0, 1.0, 0.0, 0.1, 100.0);
        let _persp = cam.proj(16.0 / 9.0);
        cam.mode = CameraMode::Orthographic { half_height: 1.0 };
        let ortho = cam.proj(16.0 / 9.0);
        // Ortho-specific signature: [3][3] (perspective stores -1 there for
        // perspective divide, ortho stores 1.0)
        assert!((ortho[2][3] - 0.0).abs() < 1e-5);
    }
}
