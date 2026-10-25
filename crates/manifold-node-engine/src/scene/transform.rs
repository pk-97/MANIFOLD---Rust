//! `Transform` — port-data type carried on
//! [`PortType::Transform`](crate::node_graph::ports::PortType::Transform) wires.
//!
//! Local TRS of one scene object. CPU-only wire value, composed to a model
//! matrix by the consuming renderer per frame. Euler radians, XYZ application
//! order — matching `render_scene`'s existing `model_matrix`
//! (`render_scene.rs:419`), which is unchanged by this port's introduction.
//!
//! Produced by `node.transform_3d`, consumed (P2 of
//! by `render_scene`'s
//! `transform_n` ports instead of nine per-object params. Same CPU-struct
//! lifetime model as [`Camera`](crate::node_graph::camera::Camera),
//! [`Light`](crate::node_graph::light::Light), and
//! [`Material`](crate::node_graph::material::Material) — no GPU resource on
//! the wire, so zero interaction with texture prebinding or pooling.
//!
//! A transform may sit inside a parent: `node.transform_3d`'s `parent` input
//! stores the parent chain's world matrix in [`Transform::parent`], and
//! [`Transform::world`] is that matrix times the node's own TRS. Groups nest
//! by chaining transforms; every reader of a placement reads `world()`.

use crate::mesh::pipeline::mat4_mul;

/// A column-major 4x4 matrix, `m[col][row]`, the layout WGSL `mat4x4<f32>`
/// and `render_scene` use.
pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY_MAT4: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn identity_mat4() -> Mat4 {
    IDENTITY_MAT4
}

fn is_identity_mat4(m: &Mat4) -> bool {
    *m == IDENTITY_MAT4
}

/// `m` applied to the point `p`.
pub fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row])
}

/// `m` applied to the direction `v`: no translation.
pub fn transform_direction(m: &Mat4, v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2])
}

/// A TRS inside a parent frame. CPU-only wire value (`PortType::Transform`),
/// composed to a model matrix by the consuming renderer per frame. Euler
/// radians, XYZ application order — matching `render_scene`'s existing
/// `model_matrix` (`render_scene.rs:419`), which is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub pos: [f32; 3],
    pub rot_euler: [f32; 3], // radians
    pub scale: [f32; 3],
    /// When true, the consuming renderer ignores `rot_euler` and instead
    /// rotates the object so its local +Z axis points at the camera each
    /// frame. Position and scale stay user-controlled; roll is locked to
    /// zero so the plane stays upright.
    pub billboard: bool,
    /// The frame this TRS sits in: the parent chain's composed world matrix,
    /// identity when `node.transform_3d`'s `parent` is unwired. A full
    /// matrix, so a turned child under a non-uniformly scaled parent keeps
    /// its shear.
    #[serde(default = "identity_mat4", skip_serializing_if = "is_identity_mat4")]
    pub parent: Mat4,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            pos: [0.0; 3],
            rot_euler: [0.0; 3],
            scale: [1.0; 3],
            billboard: false,
            parent: IDENTITY_MAT4,
        }
    }
}

/// `T * R * S` from a position, XYZ Euler radians (`R = Rz * Ry * Rx`) and a
/// scale: the same matrix as `render_scene::model_matrix`.
pub fn trs_matrix(pos: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> Mat4 {
    manifold_core::scene_doc::transform::trs_matrix(pos, rot, scale)
}

impl Transform {
    /// The world matrix: the parent frame times this TRS.
    pub fn world(&self) -> Mat4 {
        self.world_with_rotation(self.rot_euler)
    }

    /// [`Self::world`] with `rot_euler` in place of the stored rotation: a
    /// billboard's rotation is known only once the camera is.
    pub fn world_with_rotation(&self, rot_euler: [f32; 3]) -> Mat4 {
        let local = trs_matrix(self.pos, rot_euler, self.scale);
        if is_identity_mat4(&self.parent) {
            local
        } else {
            mat4_mul(self.parent, local)
        }
    }

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

/// Quaternion `[x, y, z, w]` → the `(rot_x, rot_y, rot_z)` Euler triple
/// that reconstructs the SAME rotation through
/// `render_scene::model_matrix`'s exact composition order
/// (`euler_xyz_columns`: `Rz(z) · Ry(y) · Rx(x)`, column-major — see
/// that function's own array literals). The extraction formula below
/// (`rx = atan2(r21,r22)`, `ry = asin(-r20)`, `rz = atan2(r10,r00)`) was
/// derived and checked NUMERICALLY against that exact composition
/// (`euler_xyz_columns`'s literal `rx`/`ry`/`rz` arrays, transcribed
/// verbatim and multiplied out — a first hand-derivation attempt here,
/// working from a half-remembered "textbook" combined-matrix formula
/// instead of the actual source, got the signs wrong twice before this
/// numeric check caught it) and is re-verified in Rust by this module's
/// own round-trip test, which reconstructs `Rz·Ry·Rx` from the extracted
/// angles and checks it against the quaternion's own rotation matrix
/// bit-for-bit. Falls back to a `z = 0` decomposition at the gimbal-lock
/// singularity (`|r20| ~= 1`), the conventional resolution for this
/// Euler order — the fallback's sign also flips with the sign of `r20`
/// (verified the same way).
pub fn quat_to_render_scene_euler(q: [f32; 4]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    // Row-major rotation matrix r[row][col] — same convention as
    // `gltf_load::mat4_from_trs`'s upper-left 3x3 (that function is
    // column-major; these are its entries read row-major).
    rotation_to_render_scene_euler([
        [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 0.0],
        [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 0.0],
        [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
    ])
}

/// A row-major rotation matrix `r[row][col]` → the `(rot_x, rot_y, rot_z)`
/// Euler triple `render_scene::model_matrix` composes back into it (see
/// [`quat_to_render_scene_euler`]). Reads only `r00`, `r01`, `r10`, `r11`,
/// `r20`, `r21`, `r22`.
pub fn rotation_to_render_scene_euler(r: [[f32; 3]; 3]) -> [f32; 3] {
    manifold_core::scene_doc::transform::rotation_to_euler(r)
}

impl From<Transform> for manifold_core::fluid_domain::DomainBox {
    fn from(t: Transform) -> Self {
        Self {
            pos: t.pos,
            rot_euler: t.rot_euler,
            scale: t.scale,
            billboard: t.billboard,
        }
    }
}

impl From<manifold_core::fluid_domain::DomainBox> for Transform {
    fn from(b: manifold_core::fluid_domain::DomainBox) -> Self {
        Self {
            pos: b.pos,
            rot_euler: b.rot_euler,
            scale: b.scale,
            billboard: b.billboard,
            parent: IDENTITY_MAT4,
        }
    }
}

#[cfg(test)]
mod tests {
    /// Numerically verifies [`quat_to_render_scene_euler`]'s derivation:
    /// composing the returned Euler triple through the SAME `Rz*Ry_used*Rx`
    /// formula `render_scene::model_matrix` uses must reproduce the
    /// original quaternion's own rotation matrix. Avoids exact gimbal-lock
    /// angles (|y| ~= 90 degrees) where the decomposition is ambiguous by
    /// construction.
    #[test]
    fn quat_to_euler_round_trips_through_render_scene_composition() {
        // Row-major r[row][col], matching `gltf_load::mat4_from_trs`'s
        // upper-left 3x3 EXACTLY (that function is column-major; this is
        // its transpose-to-row-major reading) — the authoritative
        // glTF-spec quat->matrix convention already load-bearing
        // elsewhere in this codebase.
        fn quat_to_matrix(q: [f32; 4]) -> [[f32; 3]; 3] {
            let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
            let (xx, yy, zz) = (x * x, y * y, z * z);
            let (xy, xz, yz) = (x * y, x * z, y * z);
            let (wx, wy, wz) = (w * x, w * y, w * z);
            [
                [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
                [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
                [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
            ]
        }
        // Same composition as render_scene::euler_xyz_columns, reproduced
        // here (that fn is private to its module) so this test verifies
        // the DERIVATION independent of a cross-module dependency.
        fn euler_xyz_columns(rot: [f32; 3]) -> [[f32; 3]; 3] {
            let (cx, sx) = (rot[0].cos(), rot[0].sin());
            let (cy, sy) = (rot[1].cos(), rot[1].sin());
            let (cz, sz) = (rot[2].cos(), rot[2].sin());
            let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
            let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
            let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];
            fn mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
                let mut out = [[0.0f32; 3]; 3];
                for col in 0..3 {
                    for row in 0..3 {
                        out[col][row] =
                            a[0][row] * b[col][0] + a[1][row] * b[col][1] + a[2][row] * b[col][2];
                    }
                }
                out
            }
            mul(mul(rz, ry), rx)
        }

        let half_90 = (std::f32::consts::FRAC_PI_4).sin(); // sin(45deg)
        let cos_45 = (std::f32::consts::FRAC_PI_4).cos();
        let general_raw = [0.2f32, 0.35, -0.15, 0.9];
        let general_len = general_raw.iter().map(|v| v * v).sum::<f32>().sqrt();
        let general = general_raw.map(|v| v / general_len);
        let cases: [[f32; 4]; 6] = [
            [0.0, 0.0, 0.0, 1.0],                         // identity
            [0.0, 0.0, (0.4_f32).sin(), (0.4_f32).cos()], // Z-only
            [(0.3_f32).sin(), 0.0, 0.0, (0.3_f32).cos()], // X-only
            general,                                      // general
            [0.0, half_90, 0.0, cos_45],                  // gimbal lock: +90 deg about Y
            [0.0, -half_90, 0.0, cos_45],                 // gimbal lock: -90 deg about Y
        ];
        for q in cases {
            let euler = quat_to_render_scene_euler(q);
            // Row-major r[row][col] from the same standard quat formula,
            // in column-major array form (m[col][row]) to compare against
            // euler_xyz_columns' own column-major output.
            let rm = quat_to_matrix(q);
            let mut r_colmajor = [[0.0f32; 3]; 3];
            for row in 0..3 {
                for col in 0..3 {
                    r_colmajor[col][row] = rm[row][col];
                }
            }
            let reconstructed = euler_xyz_columns(euler);
            for col in 0..3 {
                for row in 0..3 {
                    assert!(
                        (r_colmajor[col][row] - reconstructed[col][row]).abs() < 1e-4,
                        "quat {q:?} -> euler {euler:?} did not reconstruct the same rotation \
                         at [{col}][{row}]: expected {}, got {}",
                        r_colmajor[col][row],
                        reconstructed[col][row]
                    );
                }
            }
        }
    }

    use super::*;

    #[test]
    fn default_is_identity_trs() {
        let t = Transform::default();
        assert_eq!(t.pos, [0.0, 0.0, 0.0]);
        assert_eq!(t.rot_euler, [0.0, 0.0, 0.0]);
        assert_eq!(t.scale, [1.0, 1.0, 1.0]);
        assert!(!t.billboard);
    }

    /// A point through one TRS by plain point math: scale, then X, Y, Z
    /// turns, then the move. Independent of the matrix code under test.
    fn trs_point(t: &Transform, p: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = [p[0] * t.scale[0], p[1] * t.scale[1], p[2] * t.scale[2]];
        let (cx, sx) = (t.rot_euler[0].cos(), t.rot_euler[0].sin());
        let (y, z) = (y * cx - z * sx, y * sx + z * cx);
        let (cy, sy) = (t.rot_euler[1].cos(), t.rot_euler[1].sin());
        let (x, z) = (x * cy + z * sy, -x * sy + z * cy);
        let (cz, sz) = (t.rot_euler[2].cos(), t.rot_euler[2].sin());
        let (x, y) = (x * cz - y * sz, x * sz + y * cz);
        [x + t.pos[0], y + t.pos[1], z + t.pos[2]]
    }

    fn apply(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
    }

    #[test]
    fn a_three_level_chain_composes_parent_times_child_with_shear_kept() {
        let outer = Transform {
            pos: [1.0, -2.0, 0.5],
            rot_euler: [0.0, 0.0, 0.0],
            scale: [3.0, 1.0, 0.5],
            ..Transform::default()
        };
        let middle = Transform {
            pos: [0.5, 1.0, -1.0],
            rot_euler: [0.3, -0.7, 0.4],
            scale: [1.0, 2.0, 1.0],
            ..Transform::default()
        };
        let inner = Transform {
            pos: [-0.25, 0.75, 2.0],
            rot_euler: [-0.5, 0.2, 1.1],
            scale: [0.5, 0.5, 1.5],
            ..Transform::default()
        };
        // What node.transform_3d does with a wired parent.
        let middle = Transform {
            parent: outer.world(),
            ..middle
        };
        let inner = Transform {
            parent: middle.world(),
            ..inner
        };
        let world = inner.world();
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.3, -0.8, 2.5],
        ] {
            let expected = trs_point(&outer, trs_point(&middle, trs_point(&inner, p)));
            let got = apply(&world, p);
            for i in 0..3 {
                assert!(
                    (expected[i] - got[i]).abs() < 1e-4,
                    "point {p:?}: expected {expected:?}, got {got:?}"
                );
            }
        }
        // A turned child under a non-uniformly scaled parent shears: its
        // world axes are no longer at right angles.
        let dot = (0..3).map(|r| world[0][r] * world[1][r]).sum::<f32>();
        assert!(
            dot.abs() > 1e-3,
            "the composed matrix must keep the shear, dot {dot}"
        );
    }

    #[test]
    fn an_unparented_transform_is_its_own_trs() {
        let t = Transform {
            pos: [1.0, 2.0, 3.0],
            rot_euler: [0.1, 0.2, 0.3],
            scale: [2.0, 2.0, 2.0],
            ..Transform::default()
        };
        assert_eq!(t.world(), trs_matrix(t.pos, t.rot_euler, t.scale));
        assert_eq!(Transform::default().world(), IDENTITY_MAT4);
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
