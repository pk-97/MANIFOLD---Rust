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

// ── 2D Simplex noise — port of ParticleCommon.cginc SimplexNoise2D ──
// Returns [0, 1] centered at 0.5 (matching Unity exactly).
// 8 fixed unit gradient directions, +10000 offset, XOR hash combining.

const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(1.0, 0.7071, 0.0, -0.7071, -1.0, -0.7071, 0.0, 0.7071);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(0.0, 0.7071, 1.0, 0.7071, 0.0, -0.7071, -1.0, -0.7071);

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

    let n0 = select(0.0, t0 * t0 * t0 * t0 * dot(g0, x0), t0 >= 0.0);
    let n1 = select(0.0, t1 * t1 * t1 * t1 * dot(g1, x1), t1 >= 0.0);
    let n2 = select(0.0, t2 * t2 * t2 * t2 * dot(g2, x2), t2 >= 0.0);

    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
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
