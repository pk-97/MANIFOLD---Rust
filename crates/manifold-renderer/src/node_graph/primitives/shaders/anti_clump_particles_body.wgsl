// node.anti_clump_particles — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT + OPTIONAL TEXTURE. Modulator-weighted Brownian kick on each live
// particle's position.xy. Matches anti_clump_particles.wgsl.
//
// ABI (buffer standalone codegen): `in` (Particle) coincident → e_in (in/out
// alias one buffer, run() binds it to read slot 1 + read_write slot 4). The
// OPTIONAL `strength_modulator` Texture2D is bound as `tex_strength_modulator` +
// shared `samp` (a dummy 1×1 when unwired), with an injected
// `use_strength_modulator: u32` flag (run() packs is_some()) — when 0 the body
// uses uniform weight 1. `frame_count` is a DERIVED u32. Element = the Particle
// struct. Returning e_in unchanged on an early-out reproduces the hand kernel's
// no-write. Self-contained hash inlined (acp_).
fn acp_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn acp_hash_float2(seed: u32) -> vec2<f32> {
    let h1 = acp_wang_hash(seed);
    let h2 = acp_wang_hash(h1);
    return vec2<f32>(f32(h1), f32(h2)) / 4294967296.0;
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    tex_strength_modulator: texture_2d<f32>,
    samp: sampler,
    strength: f32,
    active_count: i32,
    frame_count: u32,
    use_strength_modulator: u32,
) -> Element {
    var p = e_in;
    if strength <= 0.0 {
        return p;
    }
    if p.life <= 0.0 {
        return p;
    }

    let uv = vec2<f32>(p.position.x, p.position.y);
    var weight: f32 = 1.0;
    if use_strength_modulator != 0u {
        let m = textureSampleLevel(tex_strength_modulator, samp, uv, 0.0).r;
        weight = m / (1.0 + m);
    }

    let seed = idx * 1664525u + frame_count * 747796405u;
    let kick = (acp_hash_float2(seed) - 0.5) * strength * weight;

    p.position = vec3<f32>(p.position.x + kick.x, p.position.y + kick.y, 0.0);
    return p;
}
