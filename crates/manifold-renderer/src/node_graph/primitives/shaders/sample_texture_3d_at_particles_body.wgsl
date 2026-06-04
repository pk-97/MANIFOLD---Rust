// node.sample_texture_3d_at_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT + TEXTURE (3D). Trilinear-sample a vec3 Texture3D at each
// particle's position.xyz, write the RGB as the [f32;3] output. Matches
// sample_texture_3d_at_particles.wgsl bit-for-bit. The 3D sibling of
// sample_texture_at_particles.
//
// ABI (buffer standalone codegen): `particles` (Particle) coincident →
// e_particles; the `field` Texture3D is bound as `tex_field` + the shared
// `samp`. The codegen synthesizes
//   struct Element  { position:vec3, velocity:vec3, life:f32, age:f32, color:vec4 }  // Particle
//   struct Element2 { x: f32, y: f32, z: f32 }                                        // [f32;3] out
// `active_count` (= the wrapper guard = dispatch_count) is unused here.
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    tex_field: texture_3d<f32>,
    samp: sampler,
    active_count: i32,
) -> Element2 {
    let sample = textureSampleLevel(tex_field, samp, e_particles.position, 0.0).xyz;
    return Element2(sample.x, sample.y, sample.z);
}
