//! Matrix helpers for the scene renderer. Every matrix is column-major
//! `m[col][row]`, the WGSL `mat4x4<f32>` layout.

/// A column-major 4×4 model matrix from TRS params: `model = T * R * S`.
/// Rotation order matches `render_instanced_3d_mesh.wgsl`'s `euler_xyz`
/// (`R = Rz * Ry * Rx`), so render_scene and render_copies agree on what a
/// given `(rot_x, rot_y, rot_z)` means. Non-uniform scale's normal skew is
/// not corrected (no inverse-transpose), matching the shader side.
pub(crate) fn model_matrix(pos: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> [[f32; 4]; 4] {
    let r = euler_xyz_columns(rot);
    [
        [
            r[0][0] * scale[0],
            r[0][1] * scale[0],
            r[0][2] * scale[0],
            0.0,
        ],
        [
            r[1][0] * scale[1],
            r[1][1] * scale[1],
            r[1][2] * scale[1],
            0.0,
        ],
        [
            r[2][0] * scale[2],
            r[2][1] * scale[2],
            r[2][2] * scale[2],
            0.0,
        ],
        [pos[0], pos[1], pos[2], 1.0],
    ]
}

/// The 3×3 rotation for XYZ Euler angles (radians), `Rz * Ry * Rx`, in the
/// same column layout as `render_instanced_3d_mesh.wgsl`'s `euler_xyz`.
fn euler_xyz_columns(rot: [f32; 3]) -> [[f32; 3]; 3] {
    let (cx, sx) = (rot[0].cos(), rot[0].sin());
    let (cy, sy) = (rot[1].cos(), rot[1].sin());
    let (cz, sz) = (rot[2].cos(), rot[2].sin());

    let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
    let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
    let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];

    mat3_mul(mat3_mul(rz, ry), rx)
}

/// `a * b` for 3×3 matrices.
fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for col in 0..3 {
        for row in 0..3 {
            out[col][row] = a[0][row] * b[col][0] + a[1][row] * b[col][1] + a[2][row] * b[col][2];
        }
    }
    out
}

/// The inverse of a 4×4 matrix (Gauss-Jordan elimination with partial
/// pivoting). The RT shadow-ray pass rebuilds world position from the
/// opaque-depth prepass and this inverse of `view_proj`, so it must be
/// exact (`mat4_inverse_recovers_identity_for_a_real_camera`). `None` only
/// for a singular matrix, which no camera this renderer builds produces.
pub(crate) fn mat4_inverse(m: [[f32; 4]; 4]) -> Option<[[f32; 4]; 4]> {
    // Row-major augmented working copy [A | I]: `m[col][row]` -> `a[row][col]`.
    let mut a = [[0f32; 8]; 4];
    for (r, row) in a.iter_mut().enumerate() {
        for (c, col) in m.iter().enumerate() {
            row[c] = col[r];
        }
        row[4 + r] = 1.0;
    }
    for col in 0..4 {
        let mut pivot = col;
        for r in (col + 1)..4 {
            if a[r][col].abs() > a[pivot][col].abs() {
                pivot = r;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let inv_p = 1.0 / a[col][col];
        for v in &mut a[col] {
            *v *= inv_p;
        }
        for r in 0..4 {
            if r == col {
                continue;
            }
            let factor = a[r][col];
            if factor != 0.0 {
                let pivot_row = a[col];
                for c in 0..8 {
                    a[r][c] -= factor * pivot_row[c];
                }
            }
        }
    }
    let mut out = [[0f32; 4]; 4];
    for (r, row) in a.iter().enumerate() {
        for c in 0..4 {
            out[c][r] = row[4 + c];
        }
    }
    Some(out)
}
