// node.apply_radial_burst_to_particles — per-particle radial impulse
// around (point_x, point_y). Applies the radial + tangent + noise-
// perturbed-radial + falloff math directly to each particle's
// position.xy. Matches the legacy fluid_simulate's injection burst.
//
// Per live particle (life > 0):
//   delta = position.xy - point
//   dist  = length(delta)
//   guard: skip if dist > radius || dist < eps || amplitude*envelope < eps
//   t = dist / radius
//   radial = delta / dist
//   tangent = (-radial.y, radial.x)
//   falloff = (1 - t²)²
//   noise_angle = simplex_noise_2d(position * 8 + time*0.3) * PI
//   perturbed_radial = normalize(radial + (cos, sin)*noise_angle * 0.4 * t)
//   curl_profile = t * (1 - t) * 4
//   strength = amplitude * envelope * falloff * dt_scaled
//   push = perturbed_radial * strength + tangent * curl_profile * strength * 0.5
//   position += push

struct Uniforms {
    active_count: u32,
    _pad0: u32,
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius: f32,
    time_val: f32,
    dt_scaled: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
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

const PI: f32 = 3.14159265;

// ── Simplex noise 2D ── (bit-exact port of fluid_simulate.wgsl's
// inline simplex_noise_2d so the noise-perturbed radial direction
// matches legacy.)

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

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
    let amp_env = u.amplitude * u.envelope;
    if amp_env < 1.0e-4 {
        return;
    }

    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let pos = vec2<f32>(p.position.x, p.position.y);
    let inject_pt = vec2<f32>(u.point_x, u.point_y);
    let delta = pos - inject_pt;
    let dist2 = dot(delta, delta);
    let radius = max(u.radius, 1.0e-6);
    let radius2 = radius * radius;

    if dist2 >= radius2 || dist2 < 1.0e-8 {
        return;
    }

    let dist = sqrt(dist2);
    let t = dist / radius;
    let radial = delta / dist;
    let tangent = vec2<f32>(-radial.y, radial.x);

    let one_minus_t2 = 1.0 - t * t;
    let falloff = one_minus_t2 * one_minus_t2;

    let noise_angle = simplex_noise_2d(pos * 8.0 + u.time_val * 0.3) * PI;
    let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
    let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

    let curl_profile = t * (1.0 - t) * 4.0;

    let strength = amp_env * falloff * u.dt_scaled;
    let push = perturbed_radial * strength + tangent * curl_profile * strength * 0.5;

    p.position = vec3<f32>(pos.x + push.x, pos.y + push.y, 0.0);
    particles[i] = p;
}
