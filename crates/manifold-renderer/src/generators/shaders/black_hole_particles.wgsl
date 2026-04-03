// Black Hole — Particle Simulation
//
// Simulates particles orbiting under Schwarzschild gravity.
// Particles spawn at the outer disk edge, spiral inward, get consumed at horizon.
// Uses Verlet integration for energy conservation.
//
// Particle layout (64 bytes, matches compute_common.rs Particle struct):
//   position: vec3<f32> + pad
//   velocity: vec3<f32> + life: f32
//   age: f32 + pad[3]
//   color: vec4<f32>

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};

struct SimUniforms {
    active_count: u32,
    frame_count: u32,
    disk_inner: f32,
    disk_outer: f32,
    speed: f32,
    turbulence: f32,
    time_val: f32,
    dt: f32,
    inject_burst: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: SimUniforms;

// Wang hash (matches particle_common.wgsl)
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

fn hash_float3(seed: u32) -> vec3<f32> {
    let h0 = wang_hash(seed);
    let h1 = wang_hash(h0);
    let h2 = wang_hash(h1);
    return vec3<f32>(
        f32(h0) / 4294967296.0,
        f32(h1) / 4294967296.0,
        f32(h2) / 4294967296.0,
    );
}

// Simplex noise 2D (simplified — matching fluid sim pattern)
const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
    1.0, 0.7071, 0.0, -0.7071, -1.0, -0.7071, 0.0, 0.7071
);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
    0.0, 0.7071, 1.0, 0.7071, 0.0, -0.7071, -1.0, -0.7071
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
    let h1 = wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u
        ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u
        ^ u32(i.y + 1.0 + 10000.0) * 19349663u);
    let g0 = vec2<f32>(SIMPLEX_GRAD2_X[h0 & 7u], SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(SIMPLEX_GRAD2_X[h1 & 7u], SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(SIMPLEX_GRAD2_X[h2 & 7u], SIMPLEX_GRAD2_Y[h2 & 7u]);
    let t0 = 0.5 - dot(x0, x0);
    let t1 = 0.5 - dot(x1, x1);
    let t2 = 0.5 - dot(x2, x2);
    let n0 = select(0.0, t0 * t0 * t0 * t0 * dot(g0, x0), t0 >= 0.0);
    let n1 = select(0.0, t1 * t1 * t1 * t1 * dot(g1, x1), t1 >= 0.0);
    let n2 = select(0.0, t2 * t2 * t2 * t2 * dot(g2, x2), t2 >= 0.0);
    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
}

// Spawn a new particle at the disk outer edge
fn spawn_particle(idx: u32) -> Particle {
    let seed = wang_hash(idx * 1099u + params.frame_count * 7919u);
    let rng = hash_float3(seed);

    // Random angle in disk plane
    let angle = rng.x * 6.2831853;
    // Random radius biased toward outer edge (spawn zone)
    let spawn_r = params.disk_outer * (0.8 + 0.2 * rng.y);
    // Slight vertical spread
    let y_offset = (rng.z - 0.5) * 0.3;

    let pos = vec3<f32>(
        cos(angle) * spawn_r,
        y_offset,
        sin(angle) * spawn_r,
    );

    // Orbital velocity: v_circular = sqrt(0.5 * rs / r) for Schwarzschild (rs = 1)
    // Tangential direction (perpendicular to radial, in disk plane)
    let v_orbital = sqrt(0.5 / spawn_r);
    let tangent = vec3<f32>(-sin(angle), 0.0, cos(angle));

    // Add slight inward drift to spiral
    let radial = normalize(vec3<f32>(pos.x, 0.0, pos.z));
    let vel = tangent * v_orbital - radial * v_orbital * 0.02;

    var p: Particle;
    p.position = pos;
    p.velocity = vel;
    p.life = 1.0;
    p.age = 0.0;
    // Temperature color: will be overridden by display based on radius
    p.color = vec4<f32>(1.0, 0.7, 0.3, 1.0);
    return p;
}

@compute @workgroup_size(256)
fn simulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.active_count {
        return;
    }

    var p = particles[idx];

    // Dead particle — respawn
    if p.life < 0.5 {
        p = spawn_particle(idx);
        particles[idx] = p;
        return;
    }

    let pos = p.position;
    let r = length(pos);
    let dt = params.dt * params.speed;

    // ── Schwarzschild gravity ──
    // Newtonian + GR correction for stable orbits
    // a = -GM/r³ * pos + GR_correction
    // In our units (rs = 1, GM = 0.5):
    let r2 = r * r;
    let r3 = r2 * r;
    let gravity = -0.5 / r3 * pos;

    // GR correction: perihelion precession term (3 * rs * L² / (2 * r⁵))
    // This creates the characteristic frame-dragging spiral
    let L_vec = cross(pos, p.velocity);
    let L2 = dot(L_vec, L_vec);
    let gr_correction = -1.5 * L2 / (r2 * r3) * pos;

    let total_accel = gravity + gr_correction;

    // ── Turbulence (curl noise in disk plane) ──
    let noise_scale = 3.0;
    let angle = atan2(pos.z, pos.x);
    let n1 = simplex_noise_2d(vec2<f32>(
        angle * noise_scale + params.time_val * 0.2,
        r * noise_scale * 0.5,
    ));
    let n2 = simplex_noise_2d(vec2<f32>(
        r * noise_scale * 0.3 + params.time_val * 0.15,
        angle * noise_scale * 1.5 + 100.0,
    ));
    // Curl: perpendicular to gradient
    let turb_radial = (n1 - 0.5) * 2.0;
    let turb_tangential = (n2 - 0.5) * 2.0;
    let radial_dir = normalize(vec3<f32>(pos.x, 0.0, pos.z) + vec3<f32>(1e-8));
    let tangent_dir = vec3<f32>(-radial_dir.z, 0.0, radial_dir.x);
    let turbulence_force = (radial_dir * turb_radial + tangent_dir * turb_tangential
        + vec3<f32>(0.0, (n1 - 0.5) * 0.5, 0.0))
        * params.turbulence * 0.01;

    // ── Disk confinement (soft push toward y=0) ──
    let disk_push = vec3<f32>(0.0, -pos.y * 2.0, 0.0);

    // ── Velocity update (Verlet) ──
    p.velocity += (total_accel + turbulence_force + disk_push) * dt;

    // Light damping to prevent energy buildup from turbulence
    p.velocity *= 0.9995;

    // ── Position update ──
    p.position += p.velocity * dt;

    // ── Age ──
    p.age += params.dt;

    // ── Kill conditions ──
    let new_r = length(p.position);
    // Consumed by event horizon
    if new_r < 1.2 {
        p.life = 0.0;
    }
    // Escaped too far
    if new_r > params.disk_outer * 2.0 {
        p.life = 0.0;
    }
    // Too old
    if p.age > 30.0 {
        p.life = 0.0;
    }

    // ── Color based on radius (temperature) ──
    let t = clamp((new_r - params.disk_inner) / (params.disk_outer - params.disk_inner), 0.0, 1.0);
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.55, 0.15);
    let outer_col = vec3<f32>(0.6, 0.12, 0.02);
    var col: vec3<f32>;
    if t < 0.5 {
        col = mix(inner_col, mid_col, t * 2.0);
    } else {
        col = mix(mid_col, outer_col, (t - 0.5) * 2.0);
    }
    // Brighten particles near horizon (gravitational blueshift / heating)
    let heat = exp(-(new_r - 1.5) * 0.5) * 2.0;
    col += vec3<f32>(0.5, 0.3, 0.1) * heat;

    p.color = vec4<f32>(col, 1.0);

    particles[idx] = p;
}

// ── Seed pass: initialize all particles ──
@compute @workgroup_size(256)
fn seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.active_count {
        return;
    }

    let seed = wang_hash(idx * 31337u + 42u);
    let rng = hash_float3(seed);

    // Distribute across entire disk with radial weighting
    let angle = rng.x * 6.2831853;
    // Bias toward outer regions (more area there)
    let t = sqrt(rng.y); // sqrt for uniform area distribution
    let r = params.disk_inner + t * (params.disk_outer - params.disk_inner);
    let y_offset = (rng.z - 0.5) * 0.2;

    let pos = vec3<f32>(cos(angle) * r, y_offset, sin(angle) * r);

    // Circular orbital velocity
    let v_orbital = sqrt(0.5 / r);
    let tangent = vec3<f32>(-sin(angle), 0.0, cos(angle));
    let radial = normalize(vec3<f32>(pos.x, 0.0, pos.z));
    let vel = tangent * v_orbital - radial * v_orbital * 0.01;

    var p: Particle;
    p.position = pos;
    p.velocity = vel;
    p.life = 1.0;
    p.age = rng.z * 10.0; // Stagger ages so they don't all die at once
    p.color = vec4<f32>(1.0, 0.6, 0.2, 1.0);

    particles[idx] = p;
}
