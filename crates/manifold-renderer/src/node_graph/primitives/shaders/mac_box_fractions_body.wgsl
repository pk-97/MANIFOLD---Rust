fn mac_box_overlap(lo: vec3<f32>, hi: vec3<f32>, other_lo: vec3<f32>, other_hi: vec3<f32>) -> vec3<f32> {
    return max(vec3<f32>(0.0), min(hi, other_hi) - max(lo, other_lo));
}

fn body(idx: u32, count: u32,
    basin_min_x: f32, basin_min_y: f32, basin_min_z: f32,
    basin_max_x: f32, basin_max_y: f32, basin_max_z: f32,
    box_min_x: f32, box_min_y: f32, box_min_z: f32,
    box_max_x: f32, box_max_y: f32, box_max_z: f32) -> vec4<f32> {
    let basin_min = vec3<f32>(basin_min_x, basin_min_y, basin_min_z);
    let basin_max = vec3<f32>(basin_max_x, basin_max_y, basin_max_z);
    let box_min = vec3<f32>(box_min_x, box_min_y, box_min_z);
    let box_max = vec3<f32>(box_max_x, box_max_y, box_max_z);
    if (!water_finite3(basin_min) || !water_finite3(basin_max)
        || !water_finite3(box_min) || !water_finite3(box_max)
        || any(basin_min >= basin_max) || any(box_min >= box_max)) {
        return vec4<f32>(bitcast<f32>(0x7fc00000u));
    }

    let c = vec3<u32>(idx % 65u, (idx / 65u) % 65u, idx / 4225u);
    let lo = WATER_ORIGIN + WATER_H * vec3<f32>(c);
    let hi = lo + vec3<f32>(WATER_H);
    let basin_extent = mac_box_overlap(lo, hi, basin_min, basin_max);
    let fluid_lo = max(lo, basin_min);
    let fluid_hi = min(hi, basin_max);
    let blocked_extent = mac_box_overlap(fluid_lo, fluid_hi, box_min, box_max);
    var out = vec4<f32>(0.0);
    if (all(c < vec3<u32>(64u))) {
        let basin_volume = basin_extent.x * basin_extent.y * basin_extent.z;
        let blocked_volume = blocked_extent.x * blocked_extent.y * blocked_extent.z;
        // Establish exact closure geometrically. Metal FMA contraction can
        // leave a tiny residual when two equal products are subtracted.
        if (all(blocked_extent >= basin_extent)) { out.w = 0.0; }
        else { out.w = clamp((basin_volume - blocked_volume) * WATER_INV_H * WATER_INV_H * WATER_INV_H, 0.0, 1.0); }
    }
    for (var axis = 0u; axis < 3u; axis++) {
        let a = (axis + 1u) % 3u;
        let b = (axis + 2u) % 3u;
        // The face normal is at lo[axis], not the cell-center coordinate.
        // Tangential spans are the full [lo, hi] intervals in a and b.
        if (c[axis] == 0u || c[axis] == 64u || c[a] >= 64u || c[b] >= 64u
            || lo[axis] <= basin_min[axis] || lo[axis] >= basin_max[axis]) {
            continue;
        }
        let basin_area = basin_extent[a] * basin_extent[b];
        var blocked_area = 0.0;
        if (lo[axis] >= box_min[axis] && lo[axis] <= box_max[axis]) {
            if (blocked_extent[a] >= basin_extent[a] && blocked_extent[b] >= basin_extent[b]) { continue; }
            blocked_area = blocked_extent[a] * blocked_extent[b];
        }
        out[axis] = clamp((basin_area - blocked_area) * WATER_INV_H * WATER_INV_H, 0.0, 1.0);
    }
    return out;
}
