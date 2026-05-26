// node.anti_clump_particles — density-weighted Brownian kick on
// each live particle's position.xy.
//
// per particle i (life > 0):
//   d = density.r at p.position.xy (bilinear)
//   capped_density = d / (1 + d)
//   kick = (hash3(i, frame).xy − 0.5) * strength * capped_density
//   p.position.xy += kick
//
// The density weighting concentrates the kick where the density
// texture is bright (i.e. where many particles have splatted),
// preferentially shoving accumulated clumps apart. Frame seed
// reseeds the Wang hash each frame so adjacent frames produce
// decorrelated kicks (Brownian, not slow drift).

struct Uniforms {
    active_count: u32,
    frame_count: u32,
    strength: f32,
    _pad: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var density_tex: texture_2d<f32>;
@group(0) @binding(3) var density_sampler: sampler;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float2(seed: u32) -> vec2<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    return vec2<f32>(f32(h1), f32(h2)) / 4294967296.0;
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    if u.strength <= 0.0 {
        return;
    }
    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let uv = vec2<f32>(p.position.x, p.position.y);
    let d = textureSampleLevel(density_tex, density_sampler, uv, 0.0).r;
    let capped_density = d / (1.0 + d);

    let seed = i * 1664525u + u.frame_count * 747796405u;
    let kick = (hash_float2(seed) - 0.5) * u.strength * capped_density;

    p.position = vec3<f32>(p.position.x + kick.x, p.position.y + kick.y, 0.0);
    particles[i] = p;
}
