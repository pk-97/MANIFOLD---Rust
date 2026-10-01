// node.age_whitewater — fusable BUFFER body, COINCIDENT pool. FLIP's
// _updateDiffuseParticleLifetimes (diffuseparticlesimulation.cpp:2101): each
// live particle loses its type's lifetime modifier times the tick. Slots
// of kind 3 and up are empty and pass whole; a dead particle still ages, as in FLIP.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    dt: f32,
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
) -> Element {
    var out = e_pool;
    if e_pool.kind > 2u {
        return out;
    }
    let modifiers = vec3<f32>(bubble_lifetime_modifier, foam_lifetime_modifier, spray_lifetime_modifier);
    out.position_lifetime.w = e_pool.position_lifetime.w - modifiers[e_pool.kind] * dt;
    return out;
}
