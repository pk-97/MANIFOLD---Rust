// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.shape_particle_blobs — fusable BUFFER body, COINCIDENT particles. One
// spherical surface kernel per sorted particle: the particle's centre and a
// radius of particle_scale × the particle's own radius. An inactive particle
// (radius 0) writes zeros.
//
// ABI (buffer standalone codegen): `sorted` (FluidParticle) arrives as the
// element `e_sorted`; the output FluidBlob is a single vec4 (centre xyz,
// radius w).

fn body(idx: u32, count: u32, e_sorted: Element, particle_scale: f32) -> vec4<f32> {
    let p = e_sorted.position_radius;
    if !(p.w > 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(p.xyz, particle_scale * p.w);
}
