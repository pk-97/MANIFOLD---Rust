// node.mpm_grid_velocity — fusable BUFFER body. Resolves the fixed-point
// accumulation wire into grid velocities: v_i = dequantise(momentum) /
// dequantise(mass) + gravity * step_dt for nonempty cells, zero for empty
// cells, then the static-basin no-penetration projection — free-slip: only
// the normal component at a solid boundary node is removed, tangential flow
// is untouched. Output w carries the dequantised cell mass for diagnostics.
//
// ABI (buffer standalone codegen): the accumulator input is BufferGather —
// the body reads the four i32 slots of cell `idx` itself through
// `buf_accumulator` (a plain `array<i32>`; reads are safe because dispatch
// ordering guarantees both scatter stages completed). Output
// WaterGridCell is a single Vec4F channel, so the element type is
// `vec4<f32>` (no struct). Scalar params come first in the body signature
// (`cell_count` is the allocation-only convention param — ignored);
// `gravity` arrives last as a derived uniform (gravity_x/y/z, packed by
// run() from the wired scalar inputs or the default). Basin bounds are
// params.
fn body(
    idx: u32,
    count: u32,
    step_dt: f32,
    basin_min_x: f32,
    basin_min_y: f32,
    basin_min_z: f32,
    basin_max_x: f32,
    basin_max_y: f32,
    basin_max_z: f32,
    cell_count: i32,
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
