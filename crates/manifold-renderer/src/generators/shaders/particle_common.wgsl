// Shared particle/agent utilities — ported from Unity ParticleCommon.cginc.
// Include this source in compute shaders that need hash and noise functions.

// ── Particle struct (48 bytes) ──────────────────────────────────────
struct Particle {
    position: vec3<f32>,    // UV-space (0-1)
    velocity: vec3<f32>,    // per-frame velocity
    life: f32,              // 0=dead, 1=alive
    age: f32,               // seconds since spawn
    color: vec4<f32>,       // RGBA
};

// ── Physarum agent struct (16 bytes) ────────────────────────────────
struct PhysarumAgent {
    pos: vec2<f32>,         // UV-space (0-1)
    angle: f32,             // heading in radians
    _pad: f32,
};

// ── Hashing (Wang hash — deterministic, fast, no sin()) ────────────

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

fn hash_float3(seed: u32) -> vec3<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    let h3 = wang_hash(h2);
    return vec3<f32>(f32(h1), f32(h2), f32(h3)) / 4294967296.0;
}

// ── 2D Simplex noise ────────────────────────────────────────────────

fn simplex_noise_2d(p: vec2<f32>) -> f32 {
    let K1: f32 = 0.366025403784;  // (sqrt(3)-1)/2
    let K2: f32 = 0.211324865405;  // (3-sqrt(3))/6

    let i = floor(p + (p.x + p.y) * K1);
    let a = p - i + (i.x + i.y) * K2;
    let o = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), a.x > a.y);
    let b = a - o + K2;
    let c = a - 1.0 + 2.0 * K2;

    let h = max(vec3<f32>(0.5) - vec3<f32>(dot(a, a), dot(b, b), dot(c, c)), vec3<f32>(0.0));
    let h4 = h * h * h * h;

    let seed0 = u32(i.x * 73856093.0 + i.y * 19349663.0);
    let seed1 = u32((i.x + o.x) * 73856093.0 + (i.y + o.y) * 19349663.0);
    let seed2 = u32((i.x + 1.0) * 73856093.0 + (i.y + 1.0) * 19349663.0);

    let g0 = hash_float2(seed0) * 2.0 - 1.0;
    let g1 = hash_float2(seed1) * 2.0 - 1.0;
    let g2 = hash_float2(seed2) * 2.0 - 1.0;

    let n = vec3<f32>(dot(g0, a), dot(g1, b), dot(g2, c));
    return dot(h4, n) * 70.0;
}

// ── 2D Curl noise (from simplex gradient) ───────────────────────────

fn curl_noise_2d(p: vec2<f32>) -> vec2<f32> {
    let eps: f32 = 0.01;
    let n0 = simplex_noise_2d(p);
    let nx = simplex_noise_2d(p + vec2<f32>(eps, 0.0));
    let ny = simplex_noise_2d(p + vec2<f32>(0.0, eps));
    let dndx = (nx - n0) / eps;
    let dndy = (ny - n0) / eps;
    return vec2<f32>(dndy, -dndx);
}

// ── FBM (fractal Brownian motion) ───────────────────────────────────

fn fbm_noise_2d(p_in: vec2<f32>, octaves: i32) -> f32 {
    var val: f32 = 0.0;
    var amp: f32 = 0.5;
    var p = p_in;
    for (var i: i32 = 0; i < octaves; i = i + 1) {
        val += amp * simplex_noise_2d(p);
        p = p * 2.0;
        amp = amp * 0.5;
    }
    return val;
}

// ── HSV to RGB ──────────────────────────────────────────────────────

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let hh = (h % 1.0 + 1.0) % 1.0 * 6.0;
    let x = c * (1.0 - abs(hh % 2.0 - 1.0));
    var rgb: vec3<f32>;
    if hh < 1.0 { rgb = vec3<f32>(c, x, 0.0); }
    else if hh < 2.0 { rgb = vec3<f32>(x, c, 0.0); }
    else if hh < 3.0 { rgb = vec3<f32>(0.0, c, x); }
    else if hh < 4.0 { rgb = vec3<f32>(0.0, x, c); }
    else if hh < 5.0 { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    let m = v - c;
    return rgb + m;
}
