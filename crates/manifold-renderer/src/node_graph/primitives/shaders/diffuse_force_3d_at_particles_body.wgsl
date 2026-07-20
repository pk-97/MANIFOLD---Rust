// node.diffuse_force_3d_at_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT multi-input + TEXTURE. Per-particle incoherent 3D random
// kick added in place to a [f32;3] force buffer, weighted by local density
// (sampled from a Texture3D). Matches diffuse_force_3d_at_particles.wgsl.
//
// ABI (buffer standalone codegen): TWO coincident array inputs — `in` (the
// [f32;3] force, FIRST → struct Element {x,y,z}) and `particles` (Particle,
// SECOND → struct Element2) — plus the `density` Texture3D bound as `tex_density`
// + shared `samp`. `in` aliases `out` (run() binds the force buffer to the read
// slot 1 + read_write slot 5; particles=2, density=3, samp=4). `frame_count` is a
// DERIVED u32 (declared `derived_uniforms: ["frame_count:u32"]`). Returning e_in
// unchanged on an early-out reproduces the hand kernel's no-write. Self-contained
// hash inlined (dfp_).
fn dfp_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

// Independent lanes per component (seed XOR distinct constants).
fn dfp_hash_float3(seed: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(dfp_wang_hash(seed)) / 4294967296.0,
        f32(dfp_wang_hash(seed ^ 0x68bc21ebu)) / 4294967296.0,
        f32(dfp_wang_hash(seed ^ 0x02e5be93u)) / 4294967296.0,
    );
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_particles: Element2,
    tex_density: texture_3d<f32>,
    samp: sampler,
    diffusion: f32,
    active_count: i32,
    frame_count: u32,
) -> Element {
    if diffusion <= 0.0 {
        return e_in;
    }
    if e_particles.life <= 0.0 {
        return e_in;
    }

    let local_density = textureSampleLevel(tex_density, samp, e_particles.position, 0.0).r;
    let capped_density = local_density / (1.0 + local_density);

    let diff_seed = idx * 1664525u + frame_count * 747796405u;
    let kick = (dfp_hash_float3(diff_seed) - 0.5) * diffusion * capped_density;

    var f = e_in;
    f.x = f.x + kick.x;
    f.y = f.y + kick.y;
    f.z = f.z + kick.z;
    return f;
}
