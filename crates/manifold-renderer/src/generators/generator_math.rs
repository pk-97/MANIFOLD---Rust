/// Shared projection scale for line generators.
pub const PROJ_SCALE: f32 = 0.25;

/// Pseudo-random hash keyed to beat index.
/// Returns a value in [0, 1). Used by OscilloscopeXY for beat-grid ratio changes.
/// Unity ref: GeneratorMath.HashBeat
#[inline]
pub fn hash_beat(n: f32) -> f32 {
    ((n * 127.1).sin() * 43758.5453).abs().fract()
}

/// Default dot radius in normalized screen space.
pub const DEFAULT_DOT_RADIUS: f32 = 0.005;

/// 4D rotation in XY, ZW, XW planes (in-place).
#[inline]
pub fn rotate_4d(
    x: &mut f32, y: &mut f32, z: &mut f32, w: &mut f32,
    angle_xy: f32, angle_zw: f32, angle_xw: f32,
) {
    // XY plane
    let (s, c) = angle_xy.sin_cos();
    let nx = *x * c - *y * s;
    let ny = *x * s + *y * c;
    *x = nx;
    *y = ny;

    // ZW plane
    let (s, c) = angle_zw.sin_cos();
    let nz = *z * c - *w * s;
    let nw = *z * s + *w * c;
    *z = nz;
    *w = nw;

    // XW plane
    let (s, c) = angle_xw.sin_cos();
    let nx = *x * c - *w * s;
    let nw = *x * s + *w * c;
    *x = nx;
    *w = nw;
}

/// 4D -> 2D perspective projection via 3D intermediate.
/// Returns (projected_x, projected_y, depth_z) in [-PROJ_SCALE, PROJ_SCALE].
#[inline]
pub fn project_4d(x: f32, y: f32, z: f32, w: f32, proj_dist: f32) -> (f32, f32, f32) {
    let denom = proj_dist - w;
    let scale = if denom.abs() > 0.001 { proj_dist / denom } else { proj_dist / 0.001 };
    let px = x * scale;
    let py = y * scale;
    let pz = z * scale;
    (px * PROJ_SCALE, py * PROJ_SCALE, pz * PROJ_SCALE)
}

/// 3D rotation around X, Y, Z axes (in-place).
/// Takes precomputed sin/cos for each axis.
#[inline]
pub fn rotate_3d(
    x: &mut f32, y: &mut f32, z: &mut f32,
    cos_x: f32, sin_x: f32,
    cos_y: f32, sin_y: f32,
    cos_z: f32, sin_z: f32,
) {
    // Rotate around X
    let ny = *y * cos_x - *z * sin_x;
    let nz = *y * sin_x + *z * cos_x;
    *y = ny;
    *z = nz;

    // Rotate around Y
    let nx = *x * cos_y + *z * sin_y;
    let nz = -*x * sin_y + *z * cos_y;
    *x = nx;
    *z = nz;

    // Rotate around Z
    let nx = *x * cos_z - *y * sin_z;
    let ny = *x * sin_z + *y * cos_z;
    *x = nx;
    *y = ny;
}
