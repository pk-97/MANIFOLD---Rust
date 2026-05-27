// node.simplex_noise_force_at_particles — per-particle 2D simplex
// noise force, added in place to an Array<vec2<f32>> force buffer.
//
// per particle i (i < active_count):
//   uv = particles[i].position.xy
//   if has_modulator:
//       m = modulator.r at uv (bilinear)
//       capped = m / (1 + m)
//       local_amp = amplitude * (1 + capped * modulator_gain)
//   else:
//       local_amp = amplitude
//   n_x = simplex_noise_2d(uv * noise_scale + z)
//   n_y = simplex_noise_2d(uv * noise_scale + z + 100)
//   forces[i] += (vec2(n_x, n_y) - 0.5) * 2 * local_amp
//
// Domain: per-particle work, bounded by active_count, NOT by canvas
// resolution. The atom is the resolution-independent replacement for
// a per-pixel `simplex_field_2d → pack → mix` texture-domain noise
// chain — at 4K it's ~5-10× cheaper because canvas pixels can vastly
// outnumber particles. The simplex function is the same one the
// texture-domain atom uses; mathematically identical at the
// per-position level.

struct Uniforms {
    active_count: u32,
    amplitude: f32,
    modulator_gain: f32,
    z: f32,
    noise_scale: f32,
    has_modulator: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> forces: array<vec2<f32>>;
@group(0) @binding(3) var modulator_tex: texture_2d<f32>;
@group(0) @binding(4) var modulator_sampler: sampler;

// SimplexNoise2D — same formulation used in the legacy
// `fluid_simulate.wgsl` per-particle noise advection.
const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(SIMPLEX_GRAD2_X[h0 & 7u], SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(SIMPLEX_GRAD2_X[h1 & 7u], SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(SIMPLEX_GRAD2_X[h2 & 7u], SIMPLEX_GRAD2_Y[h2 & 7u]);

    let t0 = 0.5 - dot(x0, x0);
    let t1 = 0.5 - dot(x1, x1);
    let t2 = 0.5 - dot(x2, x2);

    let n0 = select(t0 * t0 * t0 * t0 * dot(g0, x0), 0.0, t0 < 0.0);
    let n1 = select(t1 * t1 * t1 * t1 * dot(g1, x1), 0.0, t1 < 0.0);
    let n2 = select(t2 * t2 * t2 * t2 * dot(g2, x2), 0.0, t2 < 0.0);

    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    let p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let uv = vec2<f32>(p.position.x, p.position.y);
    var local_amp = u.amplitude;
    if u.has_modulator != 0u {
        let m = textureSampleLevel(modulator_tex, modulator_sampler, uv, 0.0).r;
        let capped = m / (1.0 + m);
        local_amp = u.amplitude * (1.0 + capped * u.modulator_gain);
    }

    let noise_uv = uv * u.noise_scale + u.z;
    let n_x = simplex_noise_2d(noise_uv);
    let n_y = simplex_noise_2d(noise_uv + 100.0);
    let noise_force = (vec2<f32>(n_x, n_y) - 0.5) * 2.0 * local_amp;

    forces[i] = forces[i] + noise_force;
}
