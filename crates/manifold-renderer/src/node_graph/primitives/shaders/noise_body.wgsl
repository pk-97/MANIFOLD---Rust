// node.noise — fusable body (freeze §12), SOURCE. Unified 2D noise (Perlin /
// Simplex / Random) with octave fBM. Helpers verbatim from noise.wgsl; base_noise
// takes noise_type as an arg (no global uniform in a body). The Random branch
// reconstructs the hand shader's (id+0.5)*inv via (floor(uv*dims)+0.5)*inv so its
// integer UV quantisation is bit-exact at any resolution; the Perlin/Simplex
// branches use the division-uv (matching the hand). PARAMS: [type (Enum->u32, the
// body names it noise_type), scale, offset_x, offset_y, octaves (Int->i32),
// lacunarity, persistence].
fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn perlin2_hash(ix: i32, iy: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    return wang_hash(x ^ y);
}

fn perlin2_grad(h: u32) -> vec2<f32> {
    let sel = h & 7u;
    switch sel {
        case 0u: { return vec2<f32>( 1.0,  0.0); }
        case 1u: { return vec2<f32>(-1.0,  0.0); }
        case 2u: { return vec2<f32>( 0.0,  1.0); }
        case 3u: { return vec2<f32>( 0.0, -1.0); }
        case 4u: { return vec2<f32>( 0.70710677,  0.70710677); }
        case 5u: { return vec2<f32>(-0.70710677,  0.70710677); }
        case 6u: { return vec2<f32>( 0.70710677, -0.70710677); }
        default: { return vec2<f32>(-0.70710677, -0.70710677); }
    }
}

fn fade(t: f32) -> f32 { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0); }

fn perlin2(p: vec2<f32>) -> f32 {
    let i = vec2<i32>(floor(p));
    let f = p - floor(p);
    let uu = vec2<f32>(fade(f.x), fade(f.y));

    let g00 = perlin2_grad(perlin2_hash(i.x,     i.y    ));
    let g10 = perlin2_grad(perlin2_hash(i.x + 1, i.y    ));
    let g01 = perlin2_grad(perlin2_hash(i.x,     i.y + 1));
    let g11 = perlin2_grad(perlin2_hash(i.x + 1, i.y + 1));

    let n00 = dot(g00, f - vec2<f32>(0.0, 0.0));
    let n10 = dot(g10, f - vec2<f32>(1.0, 0.0));
    let n01 = dot(g01, f - vec2<f32>(0.0, 1.0));
    let n11 = dot(g11, f - vec2<f32>(1.0, 1.0));

    let nx0 = mix(n00, n10, uu.x);
    let nx1 = mix(n01, n11, uu.x);
    return mix(nx0, nx1, uu.y);
}

fn mod289_v3(x: vec3<f32>) -> vec3<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_v2(x: vec2<f32>) -> vec2<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute_v3(x: vec3<f32>) -> vec3<f32> { return mod289_v3(((x * 34.0) + 1.0) * x); }

fn snoise_2d(v_in: vec2<f32>) -> f32 {
    let C = vec4<f32>(
        0.211324865405187,
        0.366025403784439,
       -0.577350269189626,
        0.024390243902439
    );

    var v = v_in;
    var i  = floor(v + dot(v, C.yy));
    let x0 = v - i + dot(i, C.xx);

    var i1: vec2<f32>;
    if x0.x > x0.y {
        i1 = vec2<f32>(1.0, 0.0);
    } else {
        i1 = vec2<f32>(0.0, 1.0);
    }

    var x12 = x0.xyxy + C.xxzz;
    x12 = vec4<f32>(x12.xy - i1, x12.zw);

    i = mod289_v2(i);
    let p = permute_v3(
        permute_v3(i.y + vec3<f32>(0.0, i1.y, 1.0))
        + i.x + vec3<f32>(0.0, i1.x, 1.0)
    );

    var m = max(
        0.5 - vec3<f32>(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)),
        vec3<f32>(0.0)
    );
    m = m * m;
    m = m * m;

    let x  = 2.0 * fract(p * C.www) - 1.0;
    let h  = abs(x) - 0.5;
    let ox = floor(x + 0.5);
    let a0 = x - ox;

    m = m * (1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h));

    var g: vec3<f32>;
    g.x  = a0.x * x0.x  + h.x * x0.y;
    g.y  = a0.y * x12.x + h.y * x12.y;
    g.z  = a0.z * x12.z + h.z * x12.w;
    return 130.0 * dot(m, g);
}

fn base_noise(p: vec2<f32>, noise_type: u32) -> f32 {
    if noise_type == 1u {
        return snoise_2d(p);
    }
    return perlin2(p);
}

fn body(uv: vec2<f32>, dims: vec2<f32>, noise_type: u32, scale: f32, offset_x: f32, offset_y: f32, octaves: i32, lacunarity: f32, persistence: f32) -> vec4<f32> {
    // Random (white-noise hash) — R-only, octave-free. Reconstruct the hand
    // shader's (id+0.5)*inv exactly (floor(uv*dims) pins the integer).
    if noise_type == 2u {
        let inv = vec2<f32>(1.0) / dims;
        let ruv = (floor(uv * dims) + 0.5) * inv;
        let qx = u32(ruv.x * scale + offset_x);
        let qy = u32(ruv.y * scale + offset_y);
        let h = wang_hash(qx * 73856093u ^ qy * 19349663u);
        let n = f32(h) / 4294967296.0;
        return vec4<f32>(n, 0.0, 0.0, 1.0);
    }

    // Perlin / Simplex — octave-summed fBM.
    var p = uv * scale + vec2<f32>(offset_x, offset_y);
    var total = 0.0;
    var amplitude = 1.0;
    var amp_sum = 0.0;
    let max_octaves = clamp(octaves, 1, 8);
    for (var i = 0; i < max_octaves; i = i + 1) {
        total = total + base_noise(p, noise_type) * amplitude;
        amp_sum = amp_sum + amplitude;
        p = p * lacunarity;
        amplitude = amplitude * persistence;
    }
    let raw = total / max(amp_sum, 1e-5);

    var v: f32;
    if noise_type == 1u {
        v = 0.5 * (raw + 1.0);
    } else {
        v = clamp(0.5 + raw * 0.7071067811865475, 0.0, 1.0);
    }
    return vec4<f32>(v, v, v, 1.0);
}
