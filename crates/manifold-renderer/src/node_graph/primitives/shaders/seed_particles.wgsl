// node.seed_particles — emit `active_count` particles seeded in [0,1]² via
// a Wang-hash uniform pattern. Phase A.7 V1 minimal seed. Full FluidSim
// 7-pattern parity (CLT cluster, lines, rings, cross, spiral, edge) lives
// in fluid_seed.wgsl and ports onto this primitive in a follow-up session
// alongside the pattern enum param.
//
// Particle layout: WGSL packs storage buffer fields by alignment. vec3<f32>
// has align 16 (size 12); f32 has align 4. The natural field order
// `position, velocity, life, age, color` produces the exact 64-byte
// layout `compute_common.rs::Particle` uses:
//   0..12  position  | 12..16  (pad to vec3 align)
//   16..28 velocity  | 28..32  life
//   32..36 age       | 36..48  (pad to vec4 align)
//   48..64 color
//
// Excess slots beyond active_count are zeroed (life = 0).

struct SeedUniforms {
    active_count: u32,
    capacity: u32,
    seed_offset: u32,
    _pad: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: SeedUniforms;
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

fn hash_unit(seed: u32) -> f32 {
    return f32(wang_hash(seed)) / 4294967296.0;
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.capacity {
        return;
    }

    var p: Particle;
    p.velocity = vec3<f32>(0.0);
    p.age = 0.0;

    if i < params.active_count {
        let base = i * 1664525u + params.seed_offset * 747796405u;
        p.position = vec3<f32>(hash_unit(base), hash_unit(base + 1u), 0.0);
        p.life = 1.0;
        p.color = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    } else {
        p.position = vec3<f32>(0.5, 0.5, 0.0);
        p.life = 0.0;
        p.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    particles[i] = p;
}
