// node.mpm_grid_velocity — fusable BUFFER body. Resolves the fixed-point
// accumulation wire into grid velocities: v_i = dequantise(momentum) /
// dequantise(mass) + gravity * step_dt for nonempty cells, zero for empty
// cells, then static-basin and optional translating-collider no-penetration
// projections — free-slip: only the inward normal component at a solid
// boundary node is removed, tangential flow is untouched. Output w carries
// the dequantised cell mass for diagnostics.
//
// ABI (buffer standalone codegen): the accumulator input is BufferGather —
// the body reads the four i32 slots of cell `idx` itself through
// `buf_accumulator` (a plain `array<i32>`; reads are safe because dispatch
// ordering guarantees both scatter stages completed). Output
// WaterGridCell is a single Vec4F channel, so the element type is
// `vec4<f32>` (no struct). Scalar params come first in the body signature
// (`cell_count` is the allocation-only convention param — ignored);
// `collider_enabled`, `collider` and `collider_velocity` arrive as derived
// uniforms. `gravity` follows them as a derived uniform. Basin and cube bounds
// are params.
fn body(
    idx: u32,
    count: u32,
    step_dt: f32,
    cube_half_x: f32,
    cube_half_y: f32,
    cube_half_z: f32,
    basin_min_x: f32,
    basin_min_y: f32,
    basin_min_z: f32,
    basin_max_x: f32,
    basin_max_y: f32,
    basin_max_z: f32,
    cell_count: i32,
    collider_enabled: u32,
    collider: vec3<f32>,
    collider_velocity: vec3<f32>,
    gravity: vec3<f32>,
) -> vec4<f32> {
    let base = idx * 4u;
    let qm_x = buf_accumulator[base];
    let qm_y = buf_accumulator[base + 1u];
    let qm_z = buf_accumulator[base + 2u];
    let qmass = buf_accumulator[base + 3u];

    let mass = f32(qmass) / WATER_FIXED_SCALE;
    var v = vec3<f32>(0.0);
    if (mass > 0.0) {
        let momentum = vec3<f32>(f32(qm_x), f32(qm_y), f32(qm_z)) / WATER_FIXED_SCALE;
        v = momentum / mass + gravity * step_dt;
    }

    // Static basin: node positions at or outside a basin face may not move
    // into the wall. Each face clamps only its own normal component.
    let n = WATER_GRID_N;
    let node = vec3<f32>(
        f32(idx % n),
        f32((idx / n) % n),
        f32(idx / (n * n)),
    );
    let pos = WATER_ORIGIN + node * WATER_H;

    // Translating cube: use the same fixed-half-extents AABB and nearest-face
    // rule as node.water_collide_box. Grid nodes have no position projection;
    // only inward relative velocity is removed. The enable flag keeps an
    // unwired collider on the exact basin-only path.
    if (mass > 0.0 && collider_enabled != 0u) {
        let half = vec3<f32>(cube_half_x, cube_half_y, cube_half_z);
        let d = pos - collider;
        let ad = abs(d);
        if (all(ad <= half)) {
            let dx_lo = d.x + half.x;
            let dx_hi = half.x - d.x;
            let dy_lo = d.y + half.y;
            let dy_hi = half.y - d.y;
            let dz_lo = d.z + half.z;
            let dz_hi = half.z - d.z;
            var best = dx_lo;
            var axis = 0u;
            var sgn = -1.0;
            if (dx_hi < best) { best = dx_hi; axis = 0u; sgn = 1.0; }
            if (dy_lo < best) { best = dy_lo; axis = 1u; sgn = -1.0; }
            if (dy_hi < best) { best = dy_hi; axis = 1u; sgn = 1.0; }
            if (dz_lo < best) { best = dz_lo; axis = 2u; sgn = -1.0; }
            if (dz_hi < best) { best = dz_hi; axis = 2u; sgn = 1.0; }

            var n_cube = vec3<f32>(0.0);
            if (axis == 0u) {
                n_cube = vec3<f32>(sgn, 0.0, 0.0);
            } else if (axis == 1u) {
                n_cube = vec3<f32>(0.0, sgn, 0.0);
            } else {
                n_cube = vec3<f32>(0.0, 0.0, sgn);
            }
            let vn = dot(v - collider_velocity, n_cube);
            if (vn < 0.0) {
                v = v - n_cube * vn;
            }
        }
    }

    if (pos.x <= basin_min_x) {
        v.x = max(v.x, 0.0);
    }
    if (pos.x >= basin_max_x) {
        v.x = min(v.x, 0.0);
    }
    if (pos.y <= basin_min_y) {
        v.y = max(v.y, 0.0);
    }
    if (pos.y >= basin_max_y) {
        v.y = min(v.y, 0.0);
    }
    if (pos.z <= basin_min_z) {
        v.z = max(v.z, 0.0);
    }
    if (pos.z >= basin_max_z) {
        v.z = min(v.z, 0.0);
    }

    return vec4<f32>(v, mass);
}
