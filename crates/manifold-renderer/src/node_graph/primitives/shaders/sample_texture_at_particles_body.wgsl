// node.sample_texture_at_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT + TEXTURE. Bilinear-sample a 2D texture at each particle's
// position.xy, write the RG channels as the [f32;2] output. Matches
// sample_texture_at_particles.wgsl bit-for-bit.
//
// ABI (buffer standalone codegen): `particles` (Particle) is coincident →
// e_particles; the `in` Texture2D is bound as `tex_in` + the shared `samp`
// (passed as args, the buffer analogue of a texture Gather — the body samples at
// a coord it computes from the particle position). The codegen synthesizes
//   struct Element  { position:vec3, velocity:vec3, life:f32, age:f32, color:vec4 }  // Particle
//   struct Element2 { x: f32, y: f32 }                                                // [f32;2] out
// `active_count` (= the wrapper guard = dispatch_count) is unused here.
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    tex_in: texture_2d<f32>,
    samp: sampler,
    active_count: i32,
) -> Element2 {
    let uv = vec2<f32>(e_particles.position.x, e_particles.position.y);
    let sample = textureSampleLevel(tex_in, samp, uv, 0.0);
    return Element2(sample.x, sample.y);
}
