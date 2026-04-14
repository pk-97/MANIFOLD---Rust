// FluidParticleSimulate — port of Unity FluidParticleSimulate.compute
// Density-displacement fluid simulation (TouchDesigner Serum technique).
//
// Core mechanic:
//   1. Each frame, particles are rasterized into a density buffer (host side)
//   2. Density is blurred, gradient computed + rotated, vector field blurred (host side)
//   3. This shader samples the final blurred vector field at each particle's UV
//   4. Direct Euler integration: P_next = P_current + force * speed
//   5. Tiny noise advection prevents static clumping
//   6. Toroidal wrap: frac(P.xy + 1.0)

struct SimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    noise_amplitude: f32,
    density_noise_gain: f32,
    diffusion: f32,
    frame_count: u32,
    inject_point_x: f32,
    inject_point_y: f32,
    inject_force: f32,
    inject_phase: f32,
    time_val: f32,
    dt: f32,
    visible_count: u32,
    _pad0: u32,
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
@group(0) @binding(4) var s_density: sampler;
@group(0) @binding(5) var<uniform> params: SimUniforms;

const PI: f32 = 3.14159265;

const INJECT_FORCE_RADIUS: f32 = 0.25;

// ---- Hash functions (port of ParticleCommon.cginc) ----

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

fn hash_float2(seed: u32) -> vec2<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    return vec2<f32>(f32(h1), f32(h2)) / 4294967296.0;
}

// ---- SimplexNoise2D (mechanical port of ParticleCommon.cginc lines 72-120) ----
// 8 evenly-spaced unit gradient directions (no trig)
const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378; // (sqrt(3)-1)/2
    let G2: f32 = 0.21132486540; // (3-sqrt(3))/6

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
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let is_excess = id.x >= params.visible_count;
    var p = particles[id.x];

    // Excess + dead: hold at nozzle
    if is_excess && p.life <= 0.0 {
        let spread_hash = hash_float2(id.x * 1664525u + 12345u);
        let spread_r = spread_hash.x * 0.005;
        let spread_a = spread_hash.y * 6.28318;
        p.position = vec3<f32>(
            0.5 + cos(spread_a) * spread_r,
            0.5 + sin(spread_a) * spread_r,
            0.0,
        );
        p.velocity = vec3<f32>(0.0);
        particles[id.x] = p;
        return;
    }

    // Allowed + dead: spawn from nozzle with radial kick
    if !is_excess && p.life <= 0.0 {
        let kick_seed = id.x * 1664525u + params.frame_count * 747796405u;
        let angle = hash_float(kick_seed) * 6.28318;
        let dt_scale = params.dt * 60.0;
        let kick = 0.0001 * params.speed * dt_scale;
        p.position.x += cos(angle) * kick;
        p.position.y += sin(angle) * kick;
        p.life = 1.0;
        p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
        particles[id.x] = p;
        return;
    }

    // ── Normal simulation ──
    let current_uv = p.position.xy;

    // 1. Sample blurred vector field
    let field_force = textureSampleLevel(t_field, s_field, current_uv, 0.0).rg;

    // 2. Sample local density for adaptive noise scaling
    let local_density = textureSampleLevel(t_density, s_density, current_uv, 0.0).r;
    let capped_density = local_density / (1.0 + local_density);
    let adaptive_amp = params.noise_amplitude * (1.0 + capped_density * params.density_noise_gain);

    // 3. Simplex noise advection — prevents static clumping
    let noise_time = params.time_val * 0.1;
    let noise_uv = current_uv * 2.0;
    let advection = vec2<f32>(
        simplex_noise_2d(noise_uv + noise_time),
        simplex_noise_2d(noise_uv + noise_time + 100.0),
    );
    var force = field_force + (advection - 0.5) * 2.0 * adaptive_amp;

    // 4. Per-particle diffusion — incoherent noise, density-weighted
    let diff_seed = id.x * 1664525u + params.frame_count * 747796405u;
    let diff = (hash_float2(diff_seed) - 0.5) * params.diffusion * capped_density;
    force += diff;

    // 6. Direct Euler integration (framerate-independent)
    let dt_scale = params.dt * 60.0;
    let new_uv = current_uv + force * params.speed * dt_scale;

    // Excess particles: no wrap — die at boundary.
    // Allowed particles: toroidal wrap.
    if is_excess {
        if new_uv.x < 0.0 || new_uv.x > 1.0 || new_uv.y < 0.0 || new_uv.y > 1.0 {
            p.life = 0.0;
            particles[id.x] = p;
            return;
        }
        p.position = vec3<f32>(new_uv, 0.0);
    } else {
        p.position = vec3<f32>(fract(new_uv + 1.0), 0.0);
    }

    // 7. Injection disturbance (applied AFTER integration)
    if params.inject_force > 0.0 {
        let inject_pt = vec2<f32>(params.inject_point_x, params.inject_point_y);
        let pos = p.position.xy;
        let delta = pos - inject_pt;
        let dist2 = dot(delta, delta);
        let force_r2 = INJECT_FORCE_RADIUS * INJECT_FORCE_RADIUS;

        let attack = clamp(params.inject_phase * 10.0, 0.0, 1.0);
        let decay = exp(-params.inject_phase * 3.0);
        let envelope = attack * decay;

        if dist2 < force_r2 && dist2 > 0.0001 && envelope > 0.001 {
            let dist = sqrt(dist2);
            let t = dist / INJECT_FORCE_RADIUS;
            let radial = delta / dist;
            let tangent = vec2<f32>(-radial.y, radial.x);

            let falloff_t = 1.0 - t * t;
            let falloff = falloff_t * falloff_t;

            let noise_angle = simplex_noise_2d(pos * 8.0 + params.time_val * 0.3) * PI;
            let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
            let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

            let curl_profile = t * (1.0 - t) * 4.0;
            let curl_force = tangent * curl_profile;

            let dt_scale2 = params.dt * 60.0;
            let strength = params.inject_force * envelope * falloff * dt_scale2;
            let push = perturbed_radial * strength + curl_force * strength * 0.5;
            let inject_uv = pos + push;
            if is_excess {
                if inject_uv.x < 0.0 || inject_uv.x > 1.0 || inject_uv.y < 0.0 || inject_uv.y > 1.0 {
                    p.life = 0.0;
                    particles[id.x] = p;
                    return;
                }
                p.position = vec3<f32>(inject_uv, 0.0);
            } else {
                p.position = vec3<f32>(fract(inject_uv + 1.0), 0.0);
            }
        }
    }

    // 8. Persistent dim color — density builds from accumulation of many splats
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[id.x] = p;
}
