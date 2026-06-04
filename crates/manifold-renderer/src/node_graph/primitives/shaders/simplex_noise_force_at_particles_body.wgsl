// node.simplex_noise_force_at_particles — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT multi-input + OPTIONAL TEXTURE. Per-particle 2D simplex
// noise force added in place to a [f32;2] force buffer, optionally boosted by a
// modulator texture. Matches simplex_noise_force_at_particles.wgsl.
//
// ABI: TWO coincident inputs — `in` (force [f32;2], FIRST → Element {x,y}) and
// `particles` (Particle, SECOND → Element2). `in` aliases `out` (run() binds
// force to read slot 1 + read_write slot 5; particles=2, modulator tex=3, samp=4).
// The OPTIONAL `amplitude_modulator` Texture2D is `tex_amplitude_modulator` +
// `samp`, gated by the injected `use_amplitude_modulator: u32` flag (run() packs
// is_some(); a dummy 1×1 when unwired). No derived fields (`z` is a param).
// Bespoke simplex inlined (snf_).
const SNF_SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SNF_SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn snf_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn snf_simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = snf_wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = snf_wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = snf_wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(SNF_SIMPLEX_GRAD2_X[h0 & 7u], SNF_SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(SNF_SIMPLEX_GRAD2_X[h1 & 7u], SNF_SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(SNF_SIMPLEX_GRAD2_X[h2 & 7u], SNF_SIMPLEX_GRAD2_Y[h2 & 7u]);

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
    e_particles: Element2,
    tex_amplitude_modulator: texture_2d<f32>,
    samp: sampler,
    amplitude: f32,
    modulator_gain: f32,
    z: f32,
    noise_scale: f32,
    active_count: i32,
    use_amplitude_modulator: u32,
) -> Element {
    if e_particles.life <= 0.0 {
        return e_in;
    }

    let uv = vec2<f32>(e_particles.position.x, e_particles.position.y);
    var local_amp = amplitude;
    if use_amplitude_modulator != 0u {
        let m = textureSampleLevel(tex_amplitude_modulator, samp, uv, 0.0).r;
        let capped = m / (1.0 + m);
        local_amp = amplitude * (1.0 + capped * modulator_gain);
    }

    let noise_uv = uv * noise_scale + z;
    let n_x = snf_simplex_noise_2d(noise_uv);
    let n_y = snf_simplex_noise_2d(noise_uv + 100.0);
    let noise_force = (vec2<f32>(n_x, n_y) - 0.5) * 2.0 * local_amp;

    return Element(e_in.x + noise_force.x, e_in.y + noise_force.y);
}
