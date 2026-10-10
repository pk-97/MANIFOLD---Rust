// FLIP diffuseparticlesimulation.cpp:1989: nearest-cell influence times energy
// times wavecrest/turbulence rates; independent emitter-generation coin.
// The existing GPU marker-density compensation and per-tick rounding are retained.

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
    center_x: f32, center_y: f32, center_z: f32,
    size_x: f32, size_y: f32, size_z: f32,
    nodes_x: f32, nodes_y: f32, nodes_z: f32,
) -> u32 {
    if f32(idx) >= live_count || !(e_particles.position_radius.w > 0.0) || !(points_per_cell > 0.0) {
        return 0u;
    }
    if length(e_particles.velocity) < 1e-3 || e_energy < 1e-6 {
        return 0u;
    }
    if ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 10u) >= generation_rate { return 0u; }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = nodes - vec3<u32>(1u);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), vec3<f32>(size_x, size_y, size_z), cells);
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) { return 0u; }
    let node = ww_cell_index(vec3<u32>(q), nodes);
    if node >= arrayLength(&buf_influence) { return 0u; }
    let per_tick = buf_influence[node] * e_energy * (rate * e_wavecrest + turbulence_rate * e_turbulence) * dt * 8.0 / points_per_cell;
    if !(per_tick > 0.0) {
        return 0u;
    }
    return u32(floor(per_tick + 0.5)) * u32(max(round(ticks), 0.0));
}
