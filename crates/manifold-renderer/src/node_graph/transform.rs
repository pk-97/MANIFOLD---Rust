//! `Transform` — port-data type carried on
//! [`PortType::Transform`](crate::node_graph::ports::PortType::Transform) wires.
//!
//! Local TRS of one scene object. CPU-only wire value, composed to a model
//! matrix by the consuming renderer per frame. Euler radians, XYZ application
//! order — matching `render_scene`'s existing `model_matrix`
//! (`render_scene.rs:419`), which is unchanged by this port's introduction.
//!
//! Produced by `node.transform_3d`, consumed (P2 of
//! `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md`) by `render_scene`'s
//! `transform_n` ports instead of nine per-object params. Same CPU-struct
//! lifetime model as [`Camera`](crate::node_graph::camera::Camera),
//! [`Light`](crate::node_graph::light::Light), and
//! [`Material`](crate::node_graph::material::Material) — no GPU resource on
//! the wire, so zero interaction with texture prebinding or pooling.

/// Local TRS of one scene object. CPU-only wire value (`PortType::Transform`),
/// composed to a model matrix by the consuming renderer per frame. Euler
/// radians, XYZ application order — matching `render_scene`'s existing
/// `model_matrix` (`render_scene.rs:419`), which is unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub pos: [f32; 3],
    pub rot_euler: [f32; 3], // radians
    pub scale: [f32; 3],
    /// When true, the consuming renderer ignores `rot_euler` and instead
    /// rotates the object so its local +Z axis points at the camera each
    /// frame. Position and scale stay user-controlled; roll is locked to
    /// zero so the plane stays upright.
    pub billboard: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            pos: [0.0; 3],
            rot_euler: [0.0; 3],
            scale: [1.0; 3],
            billboard: false,
        }
    }
}

impl Transform {
    /// Euler angles (radians, XYZ order) that rotate the object so its local
    /// +Z axis points from `self.pos` toward `camera_pos`, with zero roll.
    /// Falls back to `self.rot_euler` if the camera is coincident with the
    /// object.
    pub fn billboard_rot_euler(&self, camera_pos: [f32; 3]) -> [f32; 3] {
        let dx = camera_pos[0] - self.pos[0];
        let dy = camera_pos[1] - self.pos[1];
        let dz = camera_pos[2] - self.pos[2];
        let len_sq = dx * dx + dy * dy + dz * dz;
        if len_sq < 1e-12 {
            return self.rot_euler;
        }
        let inv_len = 1.0 / len_sq.sqrt();
        let fx = dx * inv_len;
        let fy = dy * inv_len;
        let fz = dz * inv_len;

        // Verified against render_scene.rs:euler_xyz_columns / model_matrix:
        // these angles make local +Z map to (fx, fy, fz) with zero roll.
        let pitch = (-fy).asin();
        let yaw = fx.atan2(fz);
        [pitch, yaw, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity_trs() {
        let t = Transform::default();
        assert_eq!(t.pos, [0.0, 0.0, 0.0]);
        assert_eq!(t.rot_euler, [0.0, 0.0, 0.0]);
        assert_eq!(t.scale, [1.0, 1.0, 1.0]);
        assert!(!t.billboard);
    }

    #[test]
    fn transform_is_copy_and_cheap_to_clone() {
        let t = Transform::default();
        let _copy = t;
        let _another = t;
    }

    #[test]
    fn billboard_rot_euler_returns_finite_angles_with_zero_roll() {
        let t = Transform::default();
        let rot = t.billboard_rot_euler([1.0, 2.0, 3.0]);
        assert!(rot[0].is_finite());
        assert!(rot[1].is_finite());
        assert_eq!(rot[2], 0.0, "roll must stay zero");
    }
}
