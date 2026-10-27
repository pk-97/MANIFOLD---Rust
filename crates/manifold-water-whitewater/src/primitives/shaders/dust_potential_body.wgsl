// FLIP diffuseparticlesimulation.cpp:1801: dust potential near a dust-emitting
// solid. The source record is read at the node floor(q) of the particle's
// cell, the solid distance trilinearly over that cell's eight nodes and
// the turbulence at cell centres, all through the step's world-tile table
// (`tiles`, `pool_tiles` tiles). A particle whose cell lies in no stored tile
// emits nothing. A domain source needs Boundary Dust and the particle within
// 1.5 cells of the Bounds' low z face (bounds_lo_z).

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn dust_solid(c: vec3<i32>) -> f32 {
    return buf_solid[ts_cell_at(c)];
}

fn wc_turbulence(c: vec3<i32>) -> f32 {
    return buf_turbulence[ts_cell_at(c)];
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    min_turbulence: f32,
    max_turbulence: f32,
    dust_enabled: f32, boundary_dust: f32,
    bounds_lo_z: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let h = cell_size;
    let q = ww_cell_position(e_particles.position_radius.xyz, h);
    let node = vec3<i32>(floor(q));
    ts_home(ts_cell_at(node));
    if dust_enabled < 0.5 || !ts_cell_stored(node) { return 0.0; }
    let source = buf_source[ts_cell_at(node)];
    if source.kind == 0u || source.dust_strength <= 0.0 { return 0.0; }
    // A Bounds wall gives dust only within 1.5 cells of the Bounds' low z face.
    if source.kind == 1u && (boundary_dust < 0.5 || q.z - bounds_lo_z / h > 2.0) { return 0.0; }
    var clearance = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        clearance += ww_corner_weight(fract(q), corner) * dust_solid(node + vec3<i32>(ww_corner(corner)));
    }
    if clearance < 0.0 || clearance > 2.5 * h { return 0.0; }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        let w = ww_corner_weight(f, corner);
        k = k + w * wc_turbulence(c);
    }
    let minimum = 0.75 * min_turbulence;
    return source.dust_strength * (clamp(k, minimum, max_turbulence) - minimum) / (max_turbulence - minimum);
}
