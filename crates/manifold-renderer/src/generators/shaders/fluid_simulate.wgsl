// Particle simulation: sample vector field, apply turbulence/diffusion/respawn,
// Euler-integrate position. Reads force field texture, writes particle buffer.

struct SimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    wander: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    dt: f32,
    frame_count: u32,
    _pad: f32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var t_field: texture_2d<f32>;
@group(0) @binding(2) var s_field: sampler;
@group(0) @binding(3) var t_density: texture_2d<f32>;
@group(0) @binding(4) var<uniform> params: SimUniforms;

const PI: f32 = 3.14159265;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float(seed: u32) -> f32 {
    return f32(wang_hash(seed)) / 4294967296.0;
}

fn simplex_noise_2d(p: vec2<f32>) -> f32 {
    let K1: f32 = 0.366025403784;
    let K2: f32 = 0.211324865405;

    let i = floor(p + (p.x + p.y) * K1);
    let a = p - i + (i.x + i.y) * K2;
    let o = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), a.x > a.y);
    let b = a - o + K2;
    let c = a - 1.0 + 2.0 * K2;

    let h = max(vec3<f32>(0.5) - vec3<f32>(dot(a, a), dot(b, b), dot(c, c)), vec3<f32>(0.0));
    let h4 = h * h * h * h;

    let seed0 = u32(i.x * 73856093.0 + i.y * 19349663.0);
    let seed1 = u32((i.x + o.x) * 73856093.0 + (i.y + o.y) * 19349663.0);
    let seed2 = u32((i.x + 1.0) * 73856093.0 + (i.y + 1.0) * 19349663.0);

    let h2a = vec2<f32>(f32(wang_hash(seed0)), f32(wang_hash(wang_hash(seed0)))) / 4294967296.0;
    let h2b = vec2<f32>(f32(wang_hash(seed1)), f32(wang_hash(wang_hash(seed1)))) / 4294967296.0;
    let h2c = vec2<f32>(f32(wang_hash(seed2)), f32(wang_hash(wang_hash(seed2)))) / 4294967296.0;

    let g0 = h2a * 2.0 - 1.0;
    let g1 = h2b * 2.0 - 1.0;
    let g2 = h2c * 2.0 - 1.0;

    let n = vec3<f32>(dot(g0, a), dot(g1, b), dot(g2, c));
    return dot(h4, n) * 70.0;
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    var p = particles[id.x];
    let rng_base = wang_hash(id.x * 1299721u + params.frame_count * 6291469u);

    // Sample vector field at particle position
    let field_uv = vec2<f32>(
        fract(p.position.x + 1.0),
        fract(p.position.y + 1.0),
    );
    let field_force = textureSampleLevel(t_field, s_field, field_uv, 0.0).rg;

    // Sample density at particle position for density-adaptive effects
    let density_val = textureSampleLevel(t_density, s_field, field_uv, 0.0).r;
    let capped_density = min(density_val, 5.0);

    // Turbulence: simplex noise, density-adaptive amplitude
    let noise_pos = p.position.xy * 8.0 + f32(params.frame_count) * 0.01;
    let noise_x = simplex_noise_2d(noise_pos);
    let noise_y = simplex_noise_2d(noise_pos + vec2<f32>(17.0, 31.0));
    let anti_clump_gain = params.anti_clump * 10.0;
    let turb_amplitude = params.turbulence * (1.0 + capped_density * anti_clump_gain);
    let turb_force = vec2<f32>(noise_x, noise_y) * turb_amplitude;

    // Diffusion: wang hash random kick, density-adaptive
    let rng1 = wang_hash(rng_base);
    let rng2 = wang_hash(rng1);
    let diff_x = (f32(rng1) / 4294967296.0 * 2.0 - 1.0);
    let diff_y = (f32(rng2) / 4294967296.0 * 2.0 - 1.0);
    let diff_amplitude = params.wander * (1.0 + capped_density * 10.0);
    let diffusion = vec2<f32>(diff_x, diff_y) * diff_amplitude;

    // Respawn check
    let rng3 = wang_hash(rng2);
    let rng_respawn = f32(rng3) / 4294967296.0;
    var effective_respawn = params.respawn_rate;
    if params.respawn_rate > 0.0 {
        effective_respawn = params.respawn_rate * (1.0 + capped_density * params.dense_respawn / params.respawn_rate);
    }

    if p.life <= 0.0 || rng_respawn < effective_respawn {
        // Respawn at random edge position
        let rng4 = wang_hash(rng3);
        let rng5 = wang_hash(rng4);
        let rng6 = wang_hash(rng5);
        let edge = f32(rng4) / 4294967296.0;
        let t_edge = f32(rng5) / 4294967296.0;

        if edge < 0.25 {
            p.position.x = 0.0;
            p.position.y = t_edge;
        } else if edge < 0.5 {
            p.position.x = 1.0;
            p.position.y = t_edge;
        } else if edge < 0.75 {
            p.position.x = t_edge;
            p.position.y = 0.0;
        } else {
            p.position.x = t_edge;
            p.position.y = 1.0;
        }

        p.velocity = vec3<f32>(0.0);
        p.life = 0.5 + f32(rng6) / 4294967296.0 * 0.5;
        p.age = 0.0;
    } else {
        // Euler integration: toroidal wrap via fract
        let total_force = (field_force + turb_force + diffusion) * params.speed;
        p.position.x = fract(p.position.x + total_force.x * params.dt + 1.0);
        p.position.y = fract(p.position.y + total_force.y * params.dt + 1.0);

        // Update life
        p.life -= params.dt * 0.1;
        p.age += params.dt;
    }

    particles[id.x] = p;
}
