// Galactic Rock — Phase 1: Compute particle simulation.
//
// 100,000 instances processed in 5 sequential stages:
//   1. Linear distribution along X-axis
//   2. Primary jitter (Simplex noise, period 0.09, amp 0.001)
//   3. Spiral twist (bend around Z, 1800° = 10π, pivot X=1.5)
//   4. Undulating FBM displacement (period 2.0, 5 octaves, animated)
//   5. Per-instance random rotation (deterministic from index)

struct Uniforms {
    time: f32,
    instance_count: u32,
    speed: f32,
    wave_amp: f32,
    wave_freq: f32,
    twist_amount: f32,      // multiples of π (default 10.0 = 1800°)
    grain_amp: f32,
    _pad: f32,
};

struct Instance {
    pos_scale: vec4<f32>,   // xyz: position, w: scale
    rot_pad: vec4<f32>,     // xyz: rotation (radians), w: pad
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> instances: array<Instance>;

// ─── Simplex 3D Noise ──────────────────────────────────────────────
// Based on Ashima Arts simplex noise (MIT license).

fn mod289_3(x: vec3<f32>) -> vec3<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_4(x: vec4<f32>) -> vec4<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute(x: vec4<f32>) -> vec4<f32> { return mod289_4(((x * 34.0) + 10.0) * x); }
fn taylor_inv_sqrt(r: vec4<f32>) -> vec4<f32> { return 1.79284291400159 - 0.85373472095314 * r; }

fn simplex3d(v: vec3<f32>) -> f32 {
    let C = vec2<f32>(1.0 / 6.0, 1.0 / 3.0);
    let D = vec4<f32>(0.0, 0.5, 1.0, 2.0);

    // First corner
    var i = floor(v + dot(v, vec3(C.y)));
    let x0 = v - i + dot(i, vec3(C.x));

    // Other corners
    let g = step(x0.yzx, x0.xyz);
    let l = 1.0 - g;
    let i1 = min(g.xyz, l.zxy);
    let i2 = max(g.xyz, l.zxy);

    let x1 = x0 - i1 + C.x;
    let x2 = x0 - i2 + C.y;
    let x3 = x0 - D.yyy;

    // Permutations
    i = mod289_3(i);
    let p = permute(permute(permute(
        i.z + vec4<f32>(0.0, i1.z, i2.z, 1.0))
      + i.y + vec4<f32>(0.0, i1.y, i2.y, 1.0))
      + i.x + vec4<f32>(0.0, i1.x, i2.x, 1.0));

    // Gradients: 7x7 points over a square, mapped onto an octahedron.
    let ns = vec3<f32>(0.285714285714, -0.928571428571, 0.142857142857); // 1/7, -6/7, 1/14
    let j = p - 49.0 * floor(p * ns.z * ns.z);

    let x_ = floor(j * ns.z);
    let y_ = floor(j - 7.0 * x_);

    let x = x_ * ns.x + ns.y;
    let y = y_ * ns.x + ns.y;
    let h = 1.0 - abs(x) - abs(y);

    let b0 = vec4<f32>(x.xy, y.xy);
    let b1 = vec4<f32>(x.zw, y.zw);

    let s0 = floor(b0) * 2.0 + 1.0;
    let s1 = floor(b1) * 2.0 + 1.0;
    let sh = -step(h, vec4<f32>(0.0));

    let a0 = b0.xzyw + s0.xzyw * sh.xxyy;
    let a1 = b1.xzyw + s1.xzyw * sh.zzww;

    var p0 = vec3<f32>(a0.xy, h.x);
    var p1 = vec3<f32>(a0.zw, h.y);
    var p2 = vec3<f32>(a1.xy, h.z);
    var p3 = vec3<f32>(a1.zw, h.w);

    // Normalise gradients
    let norm = taylor_inv_sqrt(vec4<f32>(dot(p0, p0), dot(p1, p1), dot(p2, p2), dot(p3, p3)));
    p0 *= norm.x;
    p1 *= norm.y;
    p2 *= norm.z;
    p3 *= norm.w;

    // Mix final noise value
    var m = max(0.5 - vec4<f32>(dot(x0, x0), dot(x1, x1), dot(x2, x2), dot(x3, x3)), vec4(0.0));
    m = m * m;
    return 105.0 * dot(m * m, vec4<f32>(dot(p0, x0), dot(p1, x1), dot(p2, x2), dot(p3, x3)));
}

// ─── FBM (Fractal Brownian Motion) ─────────────────────────────────
// 5 octaves, lacunarity 1.5, gain 0.8

fn fbm(p: vec3<f32>) -> f32 {
    var val = 0.0;
    var amp = 1.0;
    var freq = 1.0;
    var total_amp = 0.0;
    for (var i = 0u; i < 5u; i++) {
        val += simplex3d(p * freq) * amp;
        total_amp += amp;
        freq *= 1.5;    // lacunarity
        amp *= 0.8;      // gain/persistence
    }
    return val / total_amp;
}

// ─── Deterministic pseudo-random from index ─────────────────────────

fn hash_u32(n: u32) -> f32 {
    var x = n;
    x ^= x >> 16u;
    x *= 0x45d9f3bu;
    x ^= x >> 16u;
    x *= 0x45d9f3bu;
    x ^= x >> 16u;
    return f32(x) / 4294967295.0;
}

fn random_rotation(idx: u32) -> vec3<f32> {
    let TAU = 6.283185307;
    return vec3<f32>(
        hash_u32(idx * 3u + 0u) * TAU,
        hash_u32(idx * 3u + 1u) * TAU,
        hash_u32(idx * 3u + 2u) * TAU,
    );
}

// ─── Main compute kernel ────────────────────────────────────────────

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.instance_count { return; }

    let count_f = f32(u.instance_count);
    let t = f32(idx) / count_f;

    // ── Stage 1: Base distribution along X-axis [-1, 1] ──
    var pos = vec3<f32>(t * 2.0 - 1.0, 0.0, 0.0);

    // ── Stage 2: Primary jitter (Simplex noise, freq 11.111, amp 0.001) ──
    let grain = u.grain_amp;
    pos += vec3<f32>(
        simplex3d(pos * 11.111),
        simplex3d(pos * 11.111 + vec3(100.0, 0.0, 0.0)),
        simplex3d(pos * 11.111 + vec3(0.0, 100.0, 0.0)),
    ) * grain;

    // ── Stage 3: Spiral twist (bend around Z-axis) ──
    // Pivot at X = 1.5. Total rotation = twist_amount * π (default 10π = 1800°).
    // Angle proportional to normalized distance from pivot.
    let pivot_x = 1.5;
    let dist_from_pivot = pos.x - pivot_x;
    let max_dist = 2.5; // max |dist| for points in [-1, 1] with pivot 1.5
    let normalized_dist = dist_from_pivot / max_dist;
    let twist_angle = normalized_dist * u.twist_amount * 3.14159265;
    let cos_a = cos(twist_angle);
    let sin_a = sin(twist_angle);
    pos = vec3<f32>(
        cos_a * pos.x - sin_a * pos.y,
        sin_a * pos.x + cos_a * pos.y,
        pos.z,
    );

    // ── Stage 4: Undulating FBM displacement ──
    let wave_freq = u.wave_freq;
    let wave_amp = u.wave_amp;
    let anim_offset = vec3<f32>(0.0, 0.0, u.time * u.speed);
    let fbm_val = vec3<f32>(
        fbm(pos * wave_freq + anim_offset),
        fbm(pos * wave_freq + anim_offset + vec3(37.0, 0.0, 0.0)),
        fbm(pos * wave_freq + anim_offset + vec3(0.0, 73.0, 0.0)),
    );
    pos += fbm_val * wave_amp;

    // ── Stage 5: Per-instance rotation ──
    let rot = random_rotation(idx);

    // ── Write output ──
    instances[idx] = Instance(
        vec4<f32>(pos, 0.03),   // uniform scale 0.03
        vec4<f32>(rot, 0.0),
    );
}
