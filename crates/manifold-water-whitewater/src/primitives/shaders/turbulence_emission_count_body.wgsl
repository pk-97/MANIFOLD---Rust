// FLIP diffuseparticlesimulation.cpp:1989: nearest-cell influence times energy
// times wavecrest/turbulence rates; independent emitter-generation coin.
// The GPU marker-density compensation is retained. The count over the
// frame's ticks is rounded down after adding a random unit
// (`ww_emission_unit`), so the count per second is the authored rate
// whatever the Sim Rate.
// The influence is the node floor(q) of the particle's cell, read through the
// step's world-tile table (`tiles`, `pool_tiles` tiles); a particle whose
// cell lies in no stored tile emits nothing.

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    e_energy: f32,
    e_wavecrest: f32,
    e_turbulence: f32,
    rate: f32,
    turbulence_rate: f32,
    generation_rate: f32, seed: f32, epoch: f32,
    points_per_cell: f32,
    ticks: f32,
    live_count: f32,
    dt: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> u32 {
    ww_pool_tiles = u32(pool_tiles);
    if f32(idx) >= live_count || !(e_particles.position_radius.w > 0.0) || !(points_per_cell > 0.0) {
        return 0u;
    }
    if length(e_particles.velocity) < 1e-3 || e_energy < 1e-6 {
        return 0u;
    }
    if ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 10u) >= generation_rate { return 0u; }
    let q = ww_cell_position(e_particles.position_radius.xyz, cell_size);
    let node = vec3<i32>(floor(q));
    ts_home(ts_cell_at(node));
    if !ts_cell_stored(node) { return 0u; }
    let per_tick = buf_influence[ts_cell_at(node)] * e_energy * (rate * e_wavecrest + turbulence_rate * e_turbulence) * dt * 8.0 / points_per_cell;
    if !(per_tick > 0.0) {
        return 0u;
    }
    return u32(floor(per_tick * max(round(ticks), 0.0) + ww_emission_unit(idx, e_particles.position_radius.xyz)));
}
