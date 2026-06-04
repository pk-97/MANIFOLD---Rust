// node.array_diffuse_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT. Per-particle hash-based random kick on Particle.velocity.
// Matches array_diffuse_particles.wgsl bit-for-bit (self-contained wang_hash /
// hash_float3 inlined, prefixed adp_ for fusion-collision safety).
//
// ABI (buffer standalone codegen): `in` (Particle) coincident → e_in; in/out
// alias one buffer (run() binds it to slots 1 and 2), so returning e_in
// unchanged when diffusion <= 0 reproduces the hand kernel's early return.
// `frame_count` is a DERIVED u32 uniform (declared `derived_uniforms:
// ["frame_count:u32"]` — an exact integer seed, NOT an f32). Element = Particle.
// The body uses `idx` in the per-particle hash seed.
fn adp_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn adp_hash_float3(seed: u32) -> vec3<f32> {
    let h1 = adp_wang_hash(seed);
    let h2 = adp_wang_hash(h1);
    let h3 = adp_wang_hash(h2);
    return vec3<f32>(f32(h1), f32(h2), f32(h3)) / 4294967296.0;
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    diffusion: f32,
    active_count: i32,
    frame_count: u32,
) -> Element {
    var p = e_in;
    if diffusion <= 0.0 {
        return p;
    }
    let seed = idx * 1664525u + frame_count * 747796405u;
    let kick = (adp_hash_float3(seed) - 0.5) * diffusion;
    p.velocity = p.velocity + kick;
    return p;
}
