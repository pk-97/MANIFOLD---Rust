// The foam mask: matte white foam lying on the water, read per render pixel
// from the mask the foam pass draws (`foam_pass.wgsl`), which holds tau, the
// foam particles' disc area per unit area at the water there times the Foam
// Coverage; independent discs cover 1 - exp(-tau) of the surface.
// `ripple.tiles.z` (`look_ripple_detail.wgsl`'s uniforms) is 1 when a mask is
// bound. This file holds only the reading, so the ripple detail can scale its
// strength by the cover without the scene shading; `look_foam.wgsl` does the
// shading.
//
// Binds 66 itself; the mesh shader includes this file but never calls
// `foam_cover_at`, so meshes bind none of it.

@group(0) @binding(66) var foam_mask: texture_2d<f32>;

// The foam cover at render pixel `frag_xy`, 0 to 1; 0 when no mask is bound.
fn foam_cover_at(frag_xy: vec2<f32>) -> f32 {
    if ripple.tiles.z == 0u {
        return 0.0;
    }
    let tau = textureLoad(foam_mask, vec2<i32>(frag_xy), 0).r;
    return 1.0 - exp(-max(tau, 0.0));
}
