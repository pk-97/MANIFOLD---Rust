// node.jitter_particles — fusable BUFFER body, COINCIDENT. FLIP's emitter
// jitter (diffuseparticlesimulation.cpp:1557): a live particle moves by a
// uniform offset in [−j, j] on each axis, j = 0.25·(1 − 1e-3)·cell_size,
// drawn from a stateless hash of (slot, seed, epoch). Velocity, radius and
// id pass through; a slot with radius 0 passes whole.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(idx: u32, count: u32, e_particles: Element, cell_size: f32, seed: f32, epoch: f32) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let reach = 0.25 * (1.0 - 1e-3) * cell_size;
    let s = bitcast<u32>(seed);
    let generation = u32(max(round(epoch), 0.0));
    var p = e_particles.position_radius;
    p.x = p.x + reach * (2.0 * ww_random(idx, s, generation, 0u) - 1.0);
    p.y = p.y + reach * (2.0 * ww_random(idx, s, generation, 1u) - 1.0);
    p.z = p.z + reach * (2.0 * ww_random(idx, s, generation, 2u) - 1.0);
    return Element(p, e_particles.velocity, e_particles.id);
}
