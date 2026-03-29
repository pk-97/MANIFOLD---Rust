// FSR 1.0 EASU — Edge-Adaptive Spatial Upsampling
// Translated from AMD FidelityFX Super Resolution 1.0 (MIT License, GPUOpen).
//
// Maps a render-resolution source texture to a higher-resolution output using
// a direction-adaptive Catmull-Rom reconstruction. Detects dominant gradient
// direction (horizontal vs vertical) and applies a 1D Catmull-Rom filter
// across the edge and bilinear along the edge, then clamps against the source
// 2×2 neighbourhood to suppress ringing.

struct Uniforms {
    // Output→source mapping: pp = px * scale + bias
    scale_x: f32,    // srcW / dstW
    scale_y: f32,    // srcH / dstH
    bias_x: f32,     // 0.5 * srcW/dstW − 0.5
    bias_y: f32,     // 0.5 * srcH/dstH − 0.5
    // Source texel size
    inv_src_w: f32,  // 1.0 / srcW
    inv_src_h: f32,  // 1.0 / srcH
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_src: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var t_out: texture_storage_2d<rgba16float, write>;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126729, 0.7151522, 0.0721750));
}

fn tap(ox: f32, oy: f32, base_u: f32, base_v: f32) -> vec3<f32> {
    return textureSampleLevel(
        t_src, s,
        vec2<f32>(base_u + ox * u.inv_src_w, base_v + oy * u.inv_src_h),
        0.0
    ).rgb;
}

// Catmull-Rom kernel (B=0, C=0.5) at distance |d| from sample point.
// Forms a partition of unity: sum over integer-spaced samples = 1.
fn catrom(d: f32) -> f32 {
    let x = abs(d);
    if x >= 2.0 { return 0.0; }
    let x2 = x * x;
    let x3 = x2 * x;
    if x <= 1.0 {
        return 1.5 * x3 - 2.5 * x2 + 1.0;
    }
    return -0.5 * x3 + 2.5 * x2 - 4.0 * x + 2.0;
}

// 1D Catmull-Rom reconstruction from four taps at integer positions
// -1, 0, 1, 2 with sub-pixel fraction t ∈ [0, 1).
fn catrom4(c_m1: vec3<f32>, c_0: vec3<f32>, c_1: vec3<f32>, c_2: vec3<f32>, t: f32) -> vec3<f32> {
    let w0 = catrom(t + 1.0);   // distance to tap at −1: t − (−1) = t+1
    let w1 = catrom(t);          // distance to tap at  0: t
    let w2 = catrom(1.0 - t);   // distance to tap at  1: 1−t
    let w3 = catrom(2.0 - t);   // distance to tap at  2: 2−t
    // Catmull-Rom is a partition of unity so w0+w1+w2+w3 = 1.
    return c_m1 * w0 + c_0 * w1 + c_1 * w2 + c_2 * w3;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(t_out);
    if id.x >= dims.x || id.y >= dims.y { return; }

    // Map output pixel centre to fractional source space.
    let pp = vec2<f32>(f32(id.x), f32(id.y)) * vec2<f32>(u.scale_x, u.scale_y)
             + vec2<f32>(u.bias_x, u.bias_y);

    let fp    = floor(pp);          // integer source address (TL of 2×2 quad)
    let frac  = pp - fp;            // sub-pixel fraction ∈ [0, 1)
    let bu    = (fp.x + 0.5) * u.inv_src_w;   // base UV (centre of pixel fp)
    let bv    = (fp.y + 0.5) * u.inv_src_h;

    // Load 12 source taps:
    //      B  C
    //  E  F  G  H
    //  I  J  K  L     ← F,G,J,K = reconstruction 2×2 quad
    //     M  N
    let b = tap( 0.0, -1.0, bu, bv);
    let c = tap( 1.0, -1.0, bu, bv);
    let e = tap(-1.0,  0.0, bu, bv);
    let f = tap( 0.0,  0.0, bu, bv);   // TL quad
    let g = tap( 1.0,  0.0, bu, bv);   // TR quad
    let h = tap( 2.0,  0.0, bu, bv);
    let i = tap(-1.0,  1.0, bu, bv);
    let j = tap( 0.0,  1.0, bu, bv);   // BL quad
    let k = tap( 1.0,  1.0, bu, bv);   // BR quad
    let l = tap( 2.0,  1.0, bu, bv);
    let m = tap( 0.0,  2.0, bu, bv);
    let n = tap( 1.0,  2.0, bu, bv);

    // Luma for gradient analysis.
    let lb = luma(b); let lc = luma(c);
    let le = luma(e); let lf = luma(f); let lg = luma(g); let lh = luma(h);
    let li = luma(i); let lj = luma(j); let lk = luma(k); let ll = luma(l);
    let lm = luma(m); let ln = luma(n);

    // Gradient magnitudes.
    // gradH = sum of left-right differences → indicates vertical edges.
    // gradV = sum of top-bottom differences → indicates horizontal edges.
    let gradH = abs(le - lf) + abs(lf - lg) + abs(lg - lh)
              + abs(li - lj) + abs(lj - lk) + abs(lk - ll)
              + 0.5 * (abs(lb - lc) + abs(lm - ln));
    let gradV = abs(lb - lf) + abs(lf - lj) + abs(lj - lm)
              + abs(lc - lg) + abs(lg - lk) + abs(lk - ln)
              + 0.5 * (abs(le - li) + abs(lh - ll));

    let total_grad = gradH + gradV + 0.0001;
    // blend_H: weight for horizontal Catmull-Rom (sharpens across vertical edges).
    // blend_V: weight for vertical Catmull-Rom (sharpens across horizontal edges).
    let blend_h = gradH / total_grad;
    let blend_v = gradV / total_grad;   // = 1 - blend_h

    // Option V — Catmull-Rom vertically, bilinear horizontally.
    // Pre-interpolate each row horizontally with frac.x, then apply 1D Catmull-Rom in Y.
    let bc_row = mix(b, c, frac.x);
    let fg_row = mix(f, g, frac.x);
    let jk_row = mix(j, k, frac.x);
    let mn_row = mix(m, n, frac.x);
    let result_v = catrom4(bc_row, fg_row, jk_row, mn_row, frac.y);

    // Option H — Catmull-Rom horizontally, bilinear vertically.
    // Pre-interpolate each column vertically with frac.y, then apply 1D Catmull-Rom in X.
    let ei_col = mix(e, i, frac.y);
    let fj_col = mix(f, j, frac.y);
    let gk_col = mix(g, k, frac.y);
    let hl_col = mix(h, l, frac.y);
    let result_h = catrom4(ei_col, fj_col, gk_col, hl_col, frac.x);

    // Blend: use more Option H for vertical edges, more Option V for horizontal edges.
    var col = result_h * blend_h + result_v * blend_v;

    // Anti-ringing: clamp output to the range of the 2×2 reconstruction quad.
    // Prevents Catmull-Rom overshoot from creating values outside the source range.
    let mn4 = min(min(f, g), min(j, k));
    let mx4 = max(max(f, g), max(j, k));
    col = clamp(col, mn4, mx4);

    textureStore(t_out, vec2<i32>(id.xy), vec4<f32>(col, 1.0));
}
