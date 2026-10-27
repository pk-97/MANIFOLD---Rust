// FLIP diffuseparticlesimulation.cpp:1571, :1748: inside-set turbulence potential.
// Samples turbulence and distance at p - h/2 through the step's world-tile
// table (`tiles`, `pool_tiles` tiles): a cell in an absent tile reads the
// canonical element (distance 3h, turbulence 0, air).

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn wc_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn wc_turbulence(c: vec3<i32>) -> f32 {
    return buf_turbulence[ts_cell_at(c)];
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
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
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
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        let w = ww_corner_weight(f, corner);
        d = d + w * wc_distance(c);
        k = k + w * wc_turbulence(c);
    }
    if (abs(d) < 1.5 * h) && wc_borders_air(cell) {
        return 0.0;
    }
    return inside_enabled * (clamp(k, min_turbulence, max_turbulence) - min_turbulence) / (max_turbulence - min_turbulence);
}
