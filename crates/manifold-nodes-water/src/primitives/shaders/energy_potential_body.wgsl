// node.energy_potential — fusable BUFFER body, COINCIDENT. FLIP's energy
// potential (diffuseparticlesimulation.cpp:1768): the particle's kinetic
// energy per unit mass, ½|v|², held to [min_energy, max_energy] and scaled
// to 0..1 across it. 0 for a slot with radius 0 or an empty range.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(idx: u32, count: u32, e_particles: Element, min_energy: f32, max_energy: f32) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_energy > min_energy) {
        return 0.0;
    }
    let v = e_particles.velocity;
    let e = min(max(0.5 * dot(v, v), min_energy), max_energy);
    return (e - min_energy) / (max_energy - min_energy);
}
