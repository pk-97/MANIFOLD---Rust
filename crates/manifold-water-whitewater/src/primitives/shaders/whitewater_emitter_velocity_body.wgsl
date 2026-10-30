// FLIP diffuseparticlesimulation.cpp:1699: scale eligible surface emitter velocity.
// The distance is read trilinearly at cell centres and the air test over the
// 26 neighbours of the particle's cell, both through the step's world-tile
// table (`tiles`, `pool_tiles` tiles): a cell in an absent tile reads the
// canonical element (distance 3h, air).

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn wc_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
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
    spray_speed: f32,
    seed: f32,
    epoch: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
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
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wc_distance(base + vec3<i32>(ww_corner(corner)));
    }
    var out = e_particles;
    if abs(d) < 1.5 * h && wc_borders_air(cell) && d > -0.75 * h {
        out.velocity *= 1.0 + (spray_speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 9u);
    }
    return out;
}
