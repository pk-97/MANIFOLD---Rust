// FluidParticleSimulate — port of Unity FluidParticleSimulate.compute
// Density-displacement fluid simulation (TouchDesigner Serum technique).
//
// Core mechanic:
//   1. Each frame, particles are rasterized into a density buffer (host side)
//   2. Density is blurred, gradient computed + rotated, vector field blurred (host side)
//   3. This shader samples the final blurred vector field at each particle's UV
//   4. Direct Euler integration: P_next = P_current + force * speed
//   5. Tiny noise advection prevents static clumping
//   6. Toroidal wrap: fract(P.xy + 1.0)

struct SimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    // noise amplitude (NoiseAmplitude)
    noise_amplitude: f32,
    // density noise gain (DensityNoiseGain)
    density_noise_gain: f32,
    // diffusion amount (Diffusion)
    diffusion: f32,
    // per-frame respawn probability (RefreshRate)
    refresh_rate: f32,
    // extra respawn in dense regions (DensityRefreshScale)
    density_refresh_scale: f32,
    // color mode: 0=mono, >0=inject
    color_mode: u32,
    // monotonic frame counter
    frame_count: u32,
    // -1 = off, 0-3 = active zone index
    inject_index: i32,
    // injection force strength
    inject_force: f32,
    // injection burst progress 0->1
    inject_phase: f32,
    // clip-relative time for noise evolution
    time_val: f32,
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
@group(0) @binding(2) var t_density: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: SimUniforms;

const PI: f32 = 3.14159265;

// 4 fixed injection zone positions in UV space (matches Unity INJECT_POINTS)
const INJECT_POINTS_X: array<f32, 4> = array<f32, 4>(0.5, 0.8, 0.5, 0.2);
const INJECT_POINTS_Y: array<f32, 4> = array<f32, 4>(0.2, 0.5, 0.8, 0.5);
const INJECT_COLOR_RADIUS: f32 = 0.04;
const INJECT_FORCE_RADIUS: f32 = 0.25;

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

    // WangHash-based gradient (matches Unity ParticleCommon.cginc)
    let seed0 = wang_hash(u32(i.x * 73856093.0 + i.y * 19349663.0));
    let seed1 = wang_hash(u32((i.x + o.x) * 73856093.0 + (i.y + o.y) * 19349663.0));
    let seed2 = wang_hash(u32((i.x + 1.0) * 73856093.0 + (i.y + 1.0) * 19349663.0));

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
    let current_uv = p.position.xy;

    // 1. Sample blurred vector field (bilinear with toroidal wrap via modulo)
    let field_dims = vec2<f32>(textureDimensions(t_field));
    let field_coord = vec2<u32>(current_uv * field_dims) % vec2<u32>(field_dims);
    let field_force = textureLoad(t_field, field_coord, 0).rg;

    // 2. Sample local density for adaptive noise scaling
    //    High density = flat gradient plateau = particles trapped.
    let density_dims = vec2<f32>(textureDimensions(t_density));
    let density_coord = vec2<u32>(current_uv * density_dims) % vec2<u32>(density_dims);
    let local_density = textureLoad(t_density, density_coord, 0).r;
    // Soft clamp: 0->0, 1->0.5, inf->1  (Unity: localDensity / (1.0 + localDensity))
    let capped_density = local_density / (1.0 + local_density);
    let adaptive_amp = params.noise_amplitude * (1.0 + capped_density * params.density_noise_gain);

    // 3. Simplex noise advection — prevents static clumping
    //    Unity: noiseTime = _Time2 * 0.1, noiseUV = currentUV * 2.0
    let noise_time = params.time_val * 0.1;
    let noise_uv = current_uv * 2.0;
    let advection = vec2<f32>(
        simplex_noise_2d(noise_uv + noise_time),
        simplex_noise_2d(noise_uv + noise_time + 100.0),
    );
    var force = field_force + (advection - 0.5) * 2.0 * adaptive_amp;

    // 4. Per-particle diffusion — incoherent noise, density-weighted
    //    Unity: diffSeed = i * 1664525u + _FrameCount * 747796405u
    let diff_seed = id.x * 1664525u + params.frame_count * 747796405u;
    let diff = (hash_float2(diff_seed) - 0.5) * params.diffusion * capped_density;
    force += diff;

    // 5. Refresh: density-adaptive respawn
    //    Unity: refreshSeed = i * 196613u + _FrameCount * 2891336453u
    let refresh_seed = id.x * 196613u + params.frame_count * 2891336453u;
    let adaptive_refresh = params.refresh_rate + capped_density * params.density_refresh_scale;
    if hash_float(refresh_seed) < adaptive_refresh {
        // Respawn at RANDOM UV — not edge-only (Unity: HashFloat2(refreshSeed + 7919u))
        p.position = vec3<f32>(hash_float2(refresh_seed + 7919u), 0.0);
        p.age = -1.0; // respawned particles start uncolored
        p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);
        particles[id.x] = p;
        return;
    }

    // 6. Direct Euler integration + toroidal wrap
    //    Unity: p.position.xy = frac(currentUV + force * _Speed + 1.0)  — no * dt
    p.position = vec3<f32>(fract(current_uv + force * params.speed + 1.0), 0.0);

    // 7. Injection disturbance (applied AFTER integration)
    if params.inject_index >= 0 {
        let zone = u32(params.inject_index);
        let inject_pt = vec2<f32>(INJECT_POINTS_X[zone], INJECT_POINTS_Y[zone]);
        let pos = p.position.xy;
        let delta = pos - inject_pt;
        let dist2 = dot(delta, delta);
        let force_r2 = INJECT_FORCE_RADIUS * INJECT_FORCE_RADIUS;

        // Force envelope: fast attack (~10%), exponential decay
        let attack = clamp(params.inject_phase * 10.0, 0.0, 1.0);
        let decay = exp(-params.inject_phase * 3.0);
        let envelope = attack * decay;

        if dist2 < force_r2 && dist2 > 0.0001 && envelope > 0.001 {
            let dist = sqrt(dist2);
            let t = dist / INJECT_FORCE_RADIUS;
            let radial = delta / dist;
            let tangent = vec2<f32>(-radial.y, radial.x);

            // Smooth quartic falloff
            let falloff_t = 1.0 - t * t;
            let falloff = falloff_t * falloff_t;

            // Noise perturbation: breaks circular symmetry
            let noise_angle = simplex_noise_2d(pos * 8.0 + params.time_val * 0.3) * PI;
            let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
            let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

            // Vortex ring: curl increases toward injection front
            let curl_profile = t * (1.0 - t) * 4.0;
            let curl_force = tangent * curl_profile;

            let strength = params.inject_force * envelope * falloff;
            let push = perturbed_radial * strength + curl_force * strength * 0.5;
            p.position = vec3<f32>(fract(pos + push + 1.0), 0.0);
        }

        // Color injection: fixed-aperture, only while envelope > 0.3
        if p.age < 0.0 && envelope > 0.3 {
            let color_r = INJECT_COLOR_RADIUS;
            let d = p.position.xy - inject_pt;
            if dot(d, d) < color_r * color_r {
                // age encodes zone as (zoneIndex + 1)
                p.age = f32(params.inject_index + 1);
            }
        }
    }

    // 8. Persistent dim color — density builds from accumulation of many splats
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[id.x] = p;
}
