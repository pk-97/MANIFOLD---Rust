// Strange attractor particle simulation — GPU-parallel RK2 ODE integration.
//
// cs_simulate: 8 RK2 steps per particle per frame, escape detection, diffusion,
//              3D→2D perspective projection. 5 attractor types.
// cs_seed:     Hash-based initialization with 50 warmup steps.
//              Dispatched on attractor type change.
//
// Both entry points share one uniform struct at @binding(0) (Naga rule).

// ── Particle struct (64 bytes — matches Rust compute_common::Particle) ──

struct Particle {
    position: vec3<f32>,    // UV-space (0-1)
    velocity: vec3<f32>,    // 3D attractor state (x, y, z)
    life: f32,              // 0=dead, 1=alive
    age: f32,               // unused for attractors
    color: vec4<f32>,       // RGBA
};

// ── Uniforms (shared by both entry points) ──

struct Uniforms {
    attractor_type: u32,
    particle_count: u32,
    frame_count: u32,
    _pad0: u32,
    chaos: f32,
    cam_angle: f32,
    cam_tilt: f32,
    aspect: f32,
    diffusion: f32,
    attractor_dt: f32,
    uv_scale: f32,
    attractor_scale: f32,
    attractor_center: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

const STEPS_PER_ADVANCE: i32 = 8;
// Warmup for escape-respawn (cheap, called per particle per escape event).
const RESPAWN_WARMUP: i32 = 50;
// Minimum integration time for initial seed — ensures all attractor types
// have converged onto their manifold before the first rendered frame.
// Dynamic step count = SEED_MIN_TIME / dt, capped at 2000.
const SEED_MIN_TIME: f32 = 5.0;

// ── Hashing (Wang hash — deterministic, fast, no sin()) ──

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float3(seed: u32) -> vec3<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    let h3 = wang_hash(h2);
    return vec3<f32>(f32(h1), f32(h2), f32(h3)) / 4294967296.0;
}

// ── ODE systems ──

fn lorenz(p: vec3<f32>, c: f32) -> vec3<f32> {
    let sigma = 10.0 + c * 4.0;
    let rho = 28.0 + c * 8.0;
    let beta = 2.6667 + c * 0.5;
    return vec3<f32>(
        sigma * (p.y - p.x),
        p.x * (rho - p.z) - p.y,
        p.x * p.y - beta * p.z,
    );
}

fn rossler(p: vec3<f32>, c: f32) -> vec3<f32> {
    let a = 0.2 + c * 0.15;
    let b = 0.2 + c * 0.1;
    let cc = 5.7 + c * 3.0;
    return vec3<f32>(
        -p.y - p.z,
        p.x + a * p.y,
        b + p.z * (p.x - cc),
    );
}

fn aizawa(p: vec3<f32>, c: f32) -> vec3<f32> {
    let a = 0.95 + c * 0.1;
    let b = 0.7 + c * 0.2;
    let d = 3.5 + c * 1.0;
    let e = 0.25;
    let f = 0.1;
    return vec3<f32>(
        (p.z - b) * p.x - d * p.y,
        d * p.x + (p.z - b) * p.y,
        0.6 + a * p.z - (p.z * p.z * p.z) / 3.0
            - (p.x * p.x + p.y * p.y) * (1.0 + e * p.z)
            + f * p.z * p.x * p.x * p.x,
    );
}

fn thomas(p: vec3<f32>, c: f32) -> vec3<f32> {
    let b = 0.208186 - c * 0.05;
    return vec3<f32>(
        sin(p.y) - b * p.x,
        sin(p.z) - b * p.y,
        sin(p.x) - b * p.z,
    );
}

fn halvorsen(p: vec3<f32>, c: f32) -> vec3<f32> {
    let a = 1.89 + c * 0.5;
    return vec3<f32>(
        -a * p.x - 4.0 * p.y - 4.0 * p.z - p.y * p.y,
        -a * p.y - 4.0 * p.z - 4.0 * p.x - p.z * p.z,
        -a * p.z - 4.0 * p.x - 4.0 * p.y - p.x * p.x,
    );
}

fn ode(atype: u32, p: vec3<f32>, c: f32) -> vec3<f32> {
    switch atype {
        case 0u: { return lorenz(p, c); }
        case 1u: { return rossler(p, c); }
        case 2u: { return aizawa(p, c); }
        case 3u: { return thomas(p, c); }
        default: { return halvorsen(p, c); }
    }
}

// RK2 midpoint method — matches Unity exactly
fn attractor_step(atype: u32, p: vec3<f32>, dt: f32, c: f32) -> vec3<f32> {
    let dp = ode(atype, p, c);
    let mid = p + dp * (dt * 0.5);
    let dp2 = ode(atype, mid, c);
    return p + dp2 * dt;
}

// ── 3D → 2D perspective projection ──

fn project_point(p: vec3<f32>, center: vec3<f32>, scl: f32,
                 cam_angle: f32, tilt: f32, aspect: f32, uv_scale: f32) -> vec2<f32> {
    let q = (p - center) / scl;

    // Rotate around Y axis
    let ca = cos(cam_angle);
    let sa = sin(cam_angle);
    let rx = q.x * ca - q.z * sa;
    let rz_rot = q.x * sa + q.z * ca;

    // Tilt around X axis
    let ct = cos(tilt);
    let st = sin(tilt);
    let ry = q.y * ct - rz_rot * st;
    let rz = q.y * st + rz_rot * ct;

    // Perspective projection
    let depth = rz + 2.5;
    let persp_scale = 2.0 / (uv_scale * max(depth, 0.3));

    let sx = rx * persp_scale / aspect;
    let sy = ry * persp_scale;

    return vec2<f32>(sx * 0.5 + 0.5, sy * 0.5 + 0.5);
}

// ── Respawn helper ──

fn respawn_near_center(id: u32, center: vec3<f32>, scl: f32,
                       dt: f32, chaos: f32, atype: u32) -> vec3<f32> {
    let seed = hash_float3(id * 1664525u + 12345u) * 2.0 - 1.0;
    var state = center + seed * scl * 0.15;

    for (var w = 0; w < RESPAWN_WARMUP; w++) {
        state = attractor_step(atype, state, dt, chaos);
    }
    return state;
}

// ── cs_simulate — per-frame simulation + projection ──

@compute @workgroup_size(256, 1, 1)
fn cs_simulate(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.particle_count {
        return;
    }

    var p = particles[i];
    var state = p.velocity;
    let center = u.attractor_center;

    // 1. RK2 integration — 8 steps per frame
    for (var s = 0; s < STEPS_PER_ADVANCE; s++) {
        state = attractor_step(u.attractor_type, state, u.attractor_dt, u.chaos);
    }

    // 2. Escape detection — respawn if diverged
    let offset = state - center;
    let escape_threshold = u.attractor_scale * 100.0;
    if dot(offset, offset) > escape_threshold * escape_threshold {
        state = respawn_near_center(i, center, u.attractor_scale,
                                    u.attractor_dt, u.chaos, u.attractor_type);
        p.velocity = state;
        p.position = vec3<f32>(
            project_point(state, center, u.attractor_scale,
                          u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale),
            0.0,
        );
        p.life = 1.0;
        particles[i] = p;
        return;
    }

    // 3. Diffusion — per-particle random kick
    if u.diffusion > 0.0 {
        let diff_seed = i * 1664525u + u.frame_count * 747796405u;
        let kick = (hash_float3(diff_seed) - 0.5) * u.diffusion;
        state += kick;
    }

    // 4. Store 3D state + project to 2D UV
    p.velocity = state;
    p.position = vec3<f32>(
        project_point(state, center, u.attractor_scale,
                      u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale),
        0.0,
    );
    p.life = 1.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}

// ── cs_seed — GPU-side particle initialization with warmup ──

@compute @workgroup_size(256, 1, 1)
fn cs_seed(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.particle_count {
        return;
    }

    let center = u.attractor_center;
    let scl = u.attractor_scale;

    // Hash-based random seed near attractor center
    let seed = i * 1664525u + 747796405u;
    let rnd = hash_float3(seed) * 2.0 - 1.0;
    var state = center + rnd * scl * 0.15;

    // Warmup to escape transient (doubled dt like Unity SeedKernel).
    // Step count is time-based so all attractor types get equivalent
    // convergence time regardless of their dt (Lorenz: ~834, Thomas: ~84).
    let dt = u.attractor_dt * 2.0;
    let warmup_steps = clamp(i32(SEED_MIN_TIME / dt) + 1, RESPAWN_WARMUP, 2000);
    for (var w = 0; w < warmup_steps; w++) {
        state = attractor_step(u.attractor_type, state, dt, u.chaos);
    }

    var p: Particle;
    p.velocity = state;
    p.position = vec3<f32>(
        project_point(state, center, scl, u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale),
        0.0,
    );
    p.life = 1.0;
    p.age = 0.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}
