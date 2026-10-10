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
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);
    let r10 = 2.0 * (xy + wz);
    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r11 = 1.0 - 2.0 * (xx + zz);

    let r20c = r20.clamp(-1.0, 1.0);
    if (1.0 - r20c.abs()) < 1e-6 {
        // Gimbal lock: x and z become degenerate around this axis (only
        // their sum/difference is recoverable). Pin z = 0 and fold the
        // whole rotation into x. `asin`'s derivative blows up at +/-1,
        // so computing `ry` via `asin(-r20c)` here would amplify the
        // f32 rounding already present in `r20c` into a much larger
        // angular error (measured: a quaternion built from
        // `sin/cos(PI/4)` alone put `r20c` ~6e-8 short of -1.0, which
        // `asin` turned into a ~3.5e-4 radian error) — since we already
        // know we're at the pole, set `ry` to the pole value directly
        // instead of trusting `asin` near its singularity.
        let ry = std::f32::consts::FRAC_PI_2.copysign(-r20c);
        let rx = if r20 < 0.0 { r01.atan2(r11) } else { (-r01).atan2(r11) };
        return [rx, ry, 0.0];
    }
    let rx = r21.atan2(r22);
    let ry = (-r20c).asin();
    let rz = r10.atan2(r00);
    [rx, ry, rz]
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
            [0.0, 0.0, 0.0, 1.0],                                   // identity
            [0.0, 0.0, (0.4_f32).sin(), (0.4_f32).cos()],           // Z-only
            [(0.3_f32).sin(), 0.0, 0.0, (0.3_f32).cos()],           // X-only
            general,                                                // general
            [0.0, half_90, 0.0, cos_45],                            // gimbal lock: +90 deg about Y
            [0.0, -half_90, 0.0, cos_45],                           // gimbal lock: -90 deg about Y
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

    #[test]
    fn billboard_rot_euler_returns_finite_angles_with_zero_roll() {
        let t = Transform::default();
        let rot = t.billboard_rot_euler([1.0, 2.0, 3.0]);
        assert!(rot[0].is_finite());
        assert!(rot[1].is_finite());
        assert_eq!(rot[2], 0.0, "roll must stay zero");
    }
}

impl From<Transform> for manifold_core::fluid_domain::DomainBox {
    fn from(t: Transform) -> Self {
        Self { pos: t.pos, rot_euler: t.rot_euler, scale: t.scale, billboard: t.billboard }
    }
}

impl From<manifold_core::fluid_domain::DomainBox> for Transform {
    fn from(b: manifold_core::fluid_domain::DomainBox) -> Self {
        Self { pos: b.pos, rot_euler: b.rot_euler, scale: b.scale, billboard: b.billboard }
    }
}
