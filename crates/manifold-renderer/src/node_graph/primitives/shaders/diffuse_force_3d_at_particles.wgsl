// node.diffuse_force_3d_at_particles — per-particle incoherent 3D
// random kick added in place to an Array<[f32; 3]> force buffer,
// weighted by local density.
//
// Bit-exact with the per-particle diffusion step of the legacy fused
// fluid_simulate_3d:
//
//   capped   = density.r / (1 + density.r) at p.position (trilinear)
//   diffSeed = i * 1664525u + frame_count * 747796405u
//   forces[i] += (hash_float3(diffSeed) - 0.5) * diffusion * capped
//
// Incoherent (per-particle hash) where node.simplex_noise_force_3d is
// spatially coherent. Density weighting concentrates the kick where
// particles have clumped — an anti-clumping diffusion.

struct Uniforms {
    active_count: u32,
    frame_count: u32,
    diffusion: f32,
    _pad0: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

struct ForceVec {
    x: f32,
    y: f32,
    z: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> forces: array<ForceVec>;
@group(0) @binding(3) var density_tex: texture_3d<f32>;
@group(0) @binding(4) var density_sampler: sampler;

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
    let h0 = wang_hash(seed);
    let h1 = wang_hash(h0);
    let h2 = wang_hash(h1);
    return vec3<f32>(
        f32(h0) / 4294967296.0,
        f32(h1) / 4294967296.0,
        f32(h2) / 4294967296.0,
    );
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
    let p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let local_density = textureSampleLevel(density_tex, density_sampler, p.position, 0.0).r;
    let capped_density = local_density / (1.0 + local_density);

    let diff_seed = i * 1664525u + u.frame_count * 747796405u;
    let kick = (hash_float3(diff_seed) - 0.5) * u.diffusion * capped_density;

    var f = forces[i];
    f.x = f.x + kick.x;
    f.y = f.y + kick.y;
    f.z = f.z + kick.z;
    forces[i] = f;
}
