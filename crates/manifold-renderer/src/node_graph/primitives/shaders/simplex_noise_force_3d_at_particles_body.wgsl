// node.simplex_noise_force_3d_at_particles — fusable BUFFER body (freeze §12,
// buffer domain), COINCIDENT multi-input + TEXTURE. 3D simplex noise advection
// (noise on three orthogonal planes, density-adaptive amplitude) added in place
// to a [f32;3] force buffer. Matches simplex_noise_force_3d_at_particles.wgsl.
//
// ABI: TWO coincident inputs — `in` (force [f32;3], FIRST → Element {x,y,z}) and
// `particles` (Particle, SECOND → Element2) — plus the density Texture3D bound as
// `tex_density` + shared `samp`. `in` aliases `out` (run() binds force to read
// slot 1 + read_write slot 5; particles=2, density=3, samp=4). `time2` (= seconds)
// is a DERIVED uniform. Returning e_in unchanged for a dead particle reproduces
// the hand kernel's no-write. Bespoke simplex inlined (snf3_).
const SNF3_SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SNF3_SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn snf3_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn snf3_simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = snf3_wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = snf3_wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = snf3_wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(SNF3_SIMPLEX_GRAD2_X[h0 & 7u], SNF3_SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(SNF3_SIMPLEX_GRAD2_X[h1 & 7u], SNF3_SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(SNF3_SIMPLEX_GRAD2_X[h2 & 7u], SNF3_SIMPLEX_GRAD2_Y[h2 & 7u]);

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
    tex_density: texture_3d<f32>,
    samp: sampler,
    turbulence: f32,
    anti_clump: f32,
    active_count: i32,
    time2: f32,
) -> Element {
    if e_particles.life <= 0.0 {
        return e_in;
    }

    let pos = e_particles.position;
    let local_density = textureSampleLevel(tex_density, samp, pos, 0.0).r;
    let capped_density = local_density / (1.0 + local_density);
    let adaptive_amp = turbulence * (1.0 + capped_density * anti_clump);

    let noise_time = time2 * 0.1;
    let noise_pos = pos * 2.0;
    let nx = (snf3_simplex_noise_2d(noise_pos.yz + vec2<f32>(noise_time)) - 0.5) * 2.0;
    let ny = (snf3_simplex_noise_2d(noise_pos.xz + vec2<f32>(noise_time + 100.0)) - 0.5) * 2.0;
    let nz = (snf3_simplex_noise_2d(noise_pos.xy + vec2<f32>(noise_time + 200.0)) - 0.5) * 2.0;
    let noise_force = vec3<f32>(nx, ny, nz) * adaptive_amp;

    var f = e_in;
    f.x = f.x + noise_force.x;
    f.y = f.y + noise_force.y;
    f.z = f.z + noise_force.z;
    return f;
}
