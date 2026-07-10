// node.simplex_noise_force_3d_at_particles — per-particle 3D noise
// advection added in place to an Array<[f32; 3]> force buffer.
//
// 3D noise via SimplexNoise2D evaluated on three orthogonal planes
// (yz / xz / xy), density-adaptive amplitude. Bit-exact with the
// noise-advection step of the legacy fused fluid_simulate_3d:
//
//   local_density = density.r at p.position (trilinear)
//   capped        = local_density / (1 + local_density)
//   adaptive_amp  = turbulence * (1 + capped * anti_clump)
//   noise_time    = time2 * 0.1
//   noise_pos     = p.position * turb_scale (legacy constant: 2.0)
//   nx = (simplex2d(noise_pos.yz + noise_time)        - 0.5) * 2
//   ny = (simplex2d(noise_pos.xz + noise_time + 100)  - 0.5) * 2
//   nz = (simplex2d(noise_pos.xy + noise_time + 200)  - 0.5) * 2
//   forces[i] += vec3(nx, ny, nz) * adaptive_amp

struct Uniforms {
    active_count: u32,
    turbulence: f32,
    anti_clump: f32,
    time2: f32,
    turb_scale: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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

// SimplexNoise2D — same formulation as fluid_simulate_3d.wgsl.
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

    let pos = p.position;

    let local_density = textureSampleLevel(density_tex, density_sampler, pos, 0.0).r;
    let capped_density = local_density / (1.0 + local_density);
    let adaptive_amp = u.turbulence * (1.0 + capped_density * u.anti_clump);

    let noise_time = u.time2 * 0.1;
    let noise_pos = pos * u.turb_scale;
    let nx = (simplex_noise_2d(noise_pos.yz + vec2<f32>(noise_time)) - 0.5) * 2.0;
    let ny = (simplex_noise_2d(noise_pos.xz + vec2<f32>(noise_time + 100.0)) - 0.5) * 2.0;
    let nz = (simplex_noise_2d(noise_pos.xy + vec2<f32>(noise_time + 200.0)) - 0.5) * 2.0;
    let noise_force = vec3<f32>(nx, ny, nz) * adaptive_amp;

    var f = forces[i];
    f.x = f.x + noise_force.x;
    f.y = f.y + noise_force.y;
    f.z = f.z + noise_force.z;
    forces[i] = f;
}
