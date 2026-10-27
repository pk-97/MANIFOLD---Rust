// node.wavecrest_potential — fusable BUFFER body, COINCIDENT particles,
// GATHER distance, curvature, cells and tiles. FLIP's wavecrest potential for
// a surface particle (diffuseparticlesimulation.cpp:1571, :1718), with the
// distance and curvature read trilinearly at cell centres:
//   0 unless the particle sits within 1.5 cells of the surface and its cell
//   borders air (26 neighbours);
//   0 when it is still (every velocity component under 1e-6);
//   k = curvature × cell size, 0 below min_curvature, held at max_curvature;
//   0 when the distance's gradient vanishes or the velocity's direction
//   meets the surface normal below `sharpness` (a dot product);
//   else (k − min_curvature)/(max_curvature − min_curvature).
// 0 for a slot with radius 0. Every field is read through the step's
// world-tile table (`tiles`, `pool_tiles` tiles): a cell in an absent tile
// reads the canonical element (distance 3h, curvature 0, air).
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp and interpolation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn wc_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn wc_curvature(c: vec3<i32>) -> f32 {
    return buf_curvature[ts_cell_at(c)].value;
}

fn wc_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_curvature > min_curvature) {
        return 0.0;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
        k = k + w * wc_curvature(c);
    }
    if !(abs(d) < 1.5 * h) || !wc_borders_air(cell) {
        return 0.0;
    }
    let v = e_particles.velocity;
    if all(abs(v) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    k = k * h;
    if k < min_curvature {
        return 0.0;
    }
    k = min(k, max_curvature);
    // FLIP's trilinear gradient (interpolation.cpp:197), unscaled: corner
    // index = x + 2y + 4z.
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    let grad = vec3<f32>(gx, gy, gz);
    if all(abs(grad) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    if dot(normalize(v), normalize(grad)) < sharpness {
        return 0.0;
    }
    return (k - min_curvature) / (max_curvature - min_curvature);
}
