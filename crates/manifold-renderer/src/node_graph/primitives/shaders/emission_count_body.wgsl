// node.emission_count — fusable BUFFER body, COINCIDENT. FLIP's whitewater
// count per emitter (diffuseparticlesimulation.cpp:1989, :1918) for one
// tick, times the frame's ticks (GPU_WHITEWATER_DESIGN.md D5): each tick
// rounds rate · Ie · Iwc · dt · 8/points_per_cell to the nearest whole
// number on its own (export supplies dt = 1/60). 0 for a slot at or past live_count, a slot with radius
// 0, a velocity under 1e-3 m/s, Ie under 1e-6 or Iwc of 0.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.


fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    e_energy: f32,
    e_wavecrest: f32,
    rate: f32,
    points_per_cell: f32,
    ticks: f32,
    live_count: f32,
    dt: f32,
) -> u32 {
    if f32(idx) >= live_count || !(e_particles.position_radius.w > 0.0) || !(points_per_cell > 0.0) {
        return 0u;
    }
    if length(e_particles.velocity) < 1e-3 || e_energy < 1e-6 || !(e_wavecrest > 0.0) {
        return 0u;
    }
    let per_tick = rate * e_energy * e_wavecrest * dt * 8.0 / points_per_cell;
    if !(per_tick > 0.0) {
        return 0u;
    }
    return u32(floor(per_tick + 0.5)) * u32(max(round(ticks), 0.0));
}
