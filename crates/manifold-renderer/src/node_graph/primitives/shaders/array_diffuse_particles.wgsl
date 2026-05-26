// node.array_diffuse_particles — per-particle hash-based random kick
// on Particle.velocity (3D state field).
//
// Wang-hash + frame_count seed so adjacent frames produce independent
// kicks rather than a slow drift. `diffusion` scales the kick magnitude
// (range matches the bundled integrator: typical 0..0.05).

struct Uniforms {
    active_count: u32,
    frame_count: u32,
    diffusion: f32,
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

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float3(seed: u32) -> vec3<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    let h3 = wang_hash(h2);
    return vec3<f32>(f32(h1), f32(h2), f32(h3)) / 4294967296.0;
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    if u.diffusion <= 0.0 {
        return;
    }

    let seed = i * 1664525u + u.frame_count * 747796405u;
    let kick = (hash_float3(seed) - 0.5) * u.diffusion;

    var p = particles[i];
    p.velocity = p.velocity + kick;
    particles[i] = p;
}
