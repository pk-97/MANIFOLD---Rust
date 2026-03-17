// StrangeAttractorSimulate — port of Unity StrangeAttractorSimulate.compute
//
// CSMain:     RK2 ODE integration (8 steps/frame), escape detection, respawn, diffusion.
//             3D attractor state stored in particle.velocity; projected UV in particle.position.
//
// SeedKernel: Hash-based position seeding + 50 warmup steps.
//             Dispatched on attractor type change.
//
// 5 attractor types: 0=Lorenz, 1=Rossler, 2=Aizawa, 3=Thomas, 4=Halvorsen

// Include particle struct + hash utilities
struct Particle {
    position: vec3<f32>,   // xy = UV (0-1), z unused
    velocity: vec3<f32>,   // 3D attractor state
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;

struct Uniforms {
    // Base uniforms set by host each frame
    time: f32,
    delta_time: f32,
    beat: f32,
    particle_count: u32,
    anim_speed: f32,
    uv_scale: f32,
    // Attractor-specific uniforms
    attractor_type: u32,   // 0=Lorenz 1=Rossler 2=Aizawa 3=Thomas 4=Halvorsen
    chaos: f32,            // 0-1 tunes ODE parameters
    cam_angle: f32,        // Y-axis rotation (radians)
    cam_tilt: f32,         // X-axis tilt (radians)
    aspect: f32,           // width / height
    diffusion: f32,        // per-particle random kick amplitude
    frame_count: u32,      // monotonic frame counter for hash seeds
    attractor_dt: f32,     // per-type integration timestep (pre-scaled by animSpeed)
    center_x: f32,
    center_y: f32,
    center_z: f32,
    attractor_scale: f32,
};

@group(0) @binding(1) var<uniform> u: Uniforms;

const STEPS_PER_ADVANCE: u32 = 8u;
const WARMUP_STEPS: u32 = 50u;

// ── Wang hash (deterministic, no sin) ──

fn wang_hash(seed_in: u32) -> u32 {
    var s = seed_in;
    s = (s ^ 61u) ^ (s >> 16u);
    s = s * 9u;
    s = s ^ (s >> 4u);
    s = s * 0x27d4eb2du;
    s = s ^ (s >> 15u);
    return s;
}

fn hash_float3(seed: u32) -> vec3<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    let h3 = wang_hash(h2);
    return vec3<f32>(f32(h1), f32(h2), f32(h3)) / 4294967296.0;
}

// ── ODE Systems (Unity StrangeAttractorSimulate.compute lines 45-115) ──

fn lorenz(p: vec3<f32>, c: f32) -> vec3<f32> {
    let sigma = 10.0 + c * 4.0;
    let rho   = 28.0 + c * 8.0;
    let beta  = 2.6667 + c * 0.5;  // 8/3 ≈ 2.6667
    return vec3<f32>(
        sigma * (p.y - p.x),
        p.x * (rho - p.z) - p.y,
        p.x * p.y - beta * p.z,
    );
}

fn rossler(p: vec3<f32>, c: f32) -> vec3<f32> {
    let a  = 0.2 + c * 0.15;
    let b  = 0.2 + c * 0.1;
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
    let b = 0.208186 - c * 0.05;  // MINUS — not plus
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

// RK2 midpoint method — matches Unity AttractorStep exactly
fn attractor_step(atype: u32, p: vec3<f32>, dt: f32, c: f32) -> vec3<f32> {
    let dp  = ode(atype, p, c);
    let mid = p + dp * (dt * 0.5);
    let dp2 = ode(atype, mid, c);
    return p + dp2 * dt;
}

// ── 3D → 2D projection (Unity ProjectPoint lines 130-154) ──

fn project_point(p: vec3<f32>, center: vec3<f32>, scl: f32,
                 cam_angle: f32, tilt: f32, aspect: f32, uv_scale: f32) -> vec2<f32> {
    let q = (p - center) / scl;

    // Rotate around Y axis
    let ca = cos(cam_angle);
    let sa = sin(cam_angle);
    let rx = q.x * ca - q.z * sa;
    var rz = q.x * sa + q.z * ca;

    // Tilt around X axis
    let ct = cos(tilt);
    let st = sin(tilt);
    let ry = q.y * ct - rz * st;
    rz = q.y * st + rz * ct;

    // Perspective projection
    let depth = rz + 2.5;
    let persp_scale = 2.0 / (uv_scale * max(depth, 0.3));

    let sx = rx * persp_scale / aspect;
    let sy = ry * persp_scale;

    return vec2<f32>(sx * 0.5 + 0.5, sy * 0.5 + 0.5);
}

// ── Respawn near center (Unity RespawnNearCenter lines 161-178) ──

fn respawn_near_center(particle_idx: u32, center: vec3<f32>, scl: f32,
                       dt: f32, chaos: f32, atype: u32,
                       cam_angle: f32, tilt: f32, aspect: f32,
                       uv_scale: f32) -> Particle {
    let seed = particle_idx * 1664525u + 12345u;
    let rnd  = hash_float3(seed) * 2.0 - 1.0;
    var state = center + rnd * scl * 0.15;

    for (var w: u32 = 0u; w < WARMUP_STEPS; w = w + 1u) {
        state = attractor_step(atype, state, dt, chaos);
    }

    var p: Particle;
    p.velocity  = state;
    p.position  = vec3<f32>(project_point(state, center, scl, cam_angle, tilt, aspect, uv_scale), 0.0);
    p.life      = 1.0;
    p.age       = 0.0;
    p.color     = vec4<f32>(0.005, 0.005, 0.005, 1.0);
    return p;
}

// ── CSMain — per-frame simulation + projection ──

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.particle_count { return; }

    var p = particles[i];

    // 3D attractor state lives in velocity.xyz
    var state = p.velocity;
    let center = vec3<f32>(u.center_x, u.center_y, u.center_z);

    // 1. RK2 integration — 8 steps per frame
    for (var s: u32 = 0u; s < STEPS_PER_ADVANCE; s = s + 1u) {
        state = attractor_step(u.attractor_type, state, u.attractor_dt, u.chaos);
    }

    // 2. Escape detection — respawn if diverged (Unity lines 201-210)
    let offset           = state - center;
    let escape_threshold = u.attractor_scale * 100.0;
    if dot(offset, offset) > escape_threshold * escape_threshold {
        particles[i] = respawn_near_center(i, center, u.attractor_scale,
                                           u.attractor_dt, u.chaos, u.attractor_type,
                                           u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale);
        return;
    }

    // 3. Diffusion — per-particle random kick (Unity lines 213-218)
    if u.diffusion > 0.0 {
        let diff_seed = i * 1664525u + u.frame_count * 747796405u;
        let kick      = (hash_float3(diff_seed) - 0.5) * u.diffusion;
        state         = state + kick;
    }

    // 4. Store 3D state + project to 2D UV
    p.velocity  = state;
    p.position  = vec3<f32>(project_point(state, center, u.attractor_scale,
                                          u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale), 0.0);
    p.life      = 1.0;
    p.color     = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}

// ── SeedKernel — GPU-side particle initialization with warmup ──

@compute @workgroup_size(256, 1, 1)
fn seed_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.particle_count { return; }

    let center = vec3<f32>(u.center_x, u.center_y, u.center_z);
    let scl    = u.attractor_scale;

    // Hash-based random seed near attractor center (Unity lines 246-248)
    let seed = i * 1664525u + 747796405u;
    let rnd  = hash_float3(seed) * 2.0 - 1.0;
    var state = center + rnd * scl * 0.15;

    // Warmup to escape transient — dt doubled for faster convergence (Unity line 251)
    let dt = u.attractor_dt * 2.0;
    for (var w: u32 = 0u; w < WARMUP_STEPS; w = w + 1u) {
        state = attractor_step(u.attractor_type, state, dt, u.chaos);
    }

    var p: Particle;
    p.velocity  = state;
    p.position  = vec3<f32>(project_point(state, center, scl,
                                          u.cam_angle, u.cam_tilt, u.aspect, u.uv_scale), 0.0);
    p.life      = 1.0;
    p.age       = 0.0;
    p.color     = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}
