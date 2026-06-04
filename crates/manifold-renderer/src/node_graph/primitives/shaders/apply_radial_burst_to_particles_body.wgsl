// node.apply_radial_burst_to_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT. Per-particle radial impulse around (point_x, point_y):
// radial + tangent curl + noise-perturbed radial + (1-t²)² falloff, applied to
// position.xy. Matches apply_radial_burst_to_particles.wgsl bit-for-bit (the
// bespoke inline simplex_noise_2d is included here, prefixed arb_ so it stays
// distinct from other atoms' wang_hash variants under future fusion).
//
// ABI (buffer standalone codegen): `in` (Particle) is coincident, pre-read into
// `e_in`; in/out alias one buffer (run() binds it to both the read + read_write
// slots), so returning e_in unchanged on an early-out reproduces the hand
// kernel's no-write. `time_val` (= seconds) and `dt_scaled` (= delta*60) are TWO
// DERIVED uniforms. Element = the Particle struct. `radius` is a param but also a
// local in the hand math, so the body arg is `radius_param`.
const ARB_PI: f32 = 3.14159265;

fn arb_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

const ARB_SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const ARB_SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn arb_simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = arb_wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = arb_wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = arb_wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(ARB_SIMPLEX_GRAD2_X[h0 & 7u], ARB_SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(ARB_SIMPLEX_GRAD2_X[h1 & 7u], ARB_SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(ARB_SIMPLEX_GRAD2_X[h2 & 7u], ARB_SIMPLEX_GRAD2_Y[h2 & 7u]);

    let t0 = 0.5 - dot(x0, x0);
    let t1 = 0.5 - dot(x1, x1);
    let t2 = 0.5 - dot(x2, x2);

    let n0 = select(t0 * t0 * t0 * t0 * dot(g0, x0), 0.0, t0 < 0.0);
    let n1 = select(t1 * t1 * t1 * t1 * dot(g1, x1), 0.0, t1 < 0.0);
    let n2 = select(t2 * t2 * t2 * t2 * dot(g2, x2), 0.0, t2 < 0.0);

    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius_param: f32,
    active_count: i32,
    time_val: f32,
    dt_scaled: f32,
) -> Element {
    var p = e_in;
    let amp_env = amplitude * envelope;
    if amp_env < 1.0e-4 {
        return p;
    }
    if p.life <= 0.0 {
        return p;
    }

    let pos = vec2<f32>(p.position.x, p.position.y);
    let inject_pt = vec2<f32>(point_x, point_y);
    let delta = pos - inject_pt;
    let dist2 = dot(delta, delta);
    let radius = max(radius_param, 1.0e-6);
    let radius2 = radius * radius;

    if dist2 >= radius2 || dist2 < 1.0e-8 {
        return p;
    }

    let dist = sqrt(dist2);
    let t = dist / radius;
    let radial = delta / dist;
    let tangent = vec2<f32>(-radial.y, radial.x);

    let one_minus_t2 = 1.0 - t * t;
    let falloff = one_minus_t2 * one_minus_t2;

    let noise_angle = arb_simplex_noise_2d(pos * 8.0 + time_val * 0.3) * ARB_PI;
    let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
    let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

    let curl_profile = t * (1.0 - t) * 4.0;

    let strength = amp_env * falloff * dt_scaled;
    let push = perturbed_radial * strength + tangent * curl_profile * strength * 0.5;

    p.position = vec3<f32>(pos.x + push.x, pos.y + push.y, 0.0);
    return p;
}
