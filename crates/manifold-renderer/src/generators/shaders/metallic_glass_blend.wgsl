// Metallic Glass — Pass 1: Feedback blend (Pseudo Liquid).
//
// Replicates the TD Feedback→Composite→Blur loop:
//   1. Generate Simplex noise (amplitude 0.3, offset 0.5, Z = absTime.seconds * 0.1)
//   2. Composite(Negate, opacity 0.98): prev*(1-0.98) + |noise - prev|*0.98
//
// Blur is a separate pass (metallic_glass_blur.wgsl).

struct Uniforms {
    time: f32,
    noise_scale: f32,     // TD Noise Period (default 0.75)
    noise_speed: f32,     // TD Translate Z multiplier (default 0.1)
    feedback_decay: f32,  // TD Composite opacity (default 0.98)
    width: f32,
    height: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var feedback_prev: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

// ─── Simplex 3D Noise (Ashima Arts, MIT) ───────────────────────────

fn mod289_3(x: vec3<f32>) -> vec3<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_4(x: vec4<f32>) -> vec4<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute(x: vec4<f32>) -> vec4<f32> { return mod289_4(((x * 34.0) + 10.0) * x); }
fn taylor_inv_sqrt(r: vec4<f32>) -> vec4<f32> { return 1.79284291400159 - 0.85373472095314 * r; }

fn simplex3d(v: vec3<f32>) -> f32 {
    let C = vec2<f32>(1.0 / 6.0, 1.0 / 3.0);
    let D = vec4<f32>(0.0, 0.5, 1.0, 2.0);

    var i = floor(v + dot(v, vec3(C.y)));
    let x0 = v - i + dot(i, vec3(C.x));

    let g = step(x0.yzx, x0.xyz);
    let l = 1.0 - g;
    let i1 = min(g.xyz, l.zxy);
    let i2 = max(g.xyz, l.zxy);

    let x1 = x0 - i1 + C.x;
    let x2 = x0 - i2 + C.y;
    let x3 = x0 - D.yyy;

    i = mod289_3(i);
    let p = permute(permute(permute(
        i.z + vec4<f32>(0.0, i1.z, i2.z, 1.0))
      + i.y + vec4<f32>(0.0, i1.y, i2.y, 1.0))
      + i.x + vec4<f32>(0.0, i1.x, i2.x, 1.0));

    let ns = vec3<f32>(0.285714285714, -0.928571428571, 0.142857142857);
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

    let norm = taylor_inv_sqrt(vec4<f32>(dot(p0, p0), dot(p1, p1), dot(p2, p2), dot(p3, p3)));
    p0 *= norm.x;
    p1 *= norm.y;
    p2 *= norm.z;
    p3 *= norm.w;

    var m = max(0.5 - vec4<f32>(dot(x0, x0), dot(x1, x1), dot(x2, x2), dot(x3, x3)), vec4(0.0));
    m = m * m;
    return 105.0 * dot(m * m, vec4<f32>(dot(p0, x0), dot(p1, x1), dot(p2, x2), dot(p3, x3)));
}

// ─── Main ──────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = u32(u.width);
    let h = u32(u.height);
    if gid.x >= w || gid.y >= h { return; }

    let uv = vec2<f32>(f32(gid.x) / u.width, f32(gid.y) / u.height);

    // TD Noise TOP: period = noise_scale, amplitude = 0.3, offset = 0.5
    // Translate Z = absTime.seconds * noise_speed
    let noise_pos = vec3<f32>(
        uv.x / u.noise_scale,
        uv.y / u.noise_scale,
        u.time * u.noise_speed,
    );
    // simplex3d returns [-1, 1]. Apply TD amplitude (0.3) and offset (0.5).
    let noise_val = simplex3d(noise_pos) * 0.3 + 0.5;

    // Read previous frame's feedback (post-blur from last frame)
    let prev = textureLoad(feedback_prev, vec2<i32>(gid.xy), 0);

    // TD Composite TOP: Operation = Negate, Opacity = 0.98
    // result = prev * (1 - opacity) + |noise - prev| * opacity
    let opacity = u.feedback_decay;
    let diff = abs(vec4<f32>(noise_val) - prev);
    let result = prev * (1.0 - opacity) + diff * opacity;

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
