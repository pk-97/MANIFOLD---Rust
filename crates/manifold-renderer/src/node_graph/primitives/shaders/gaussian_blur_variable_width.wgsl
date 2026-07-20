// node.gaussian_blur_variable_width — separable Gaussian blur with
// per-pixel kernel width sampled from a Texture2D (R channel). One
// dispatch = one axis (X or Y); pair two for a full 2D blur.
//
// Two specialization knobs that the pipeline preprocessor replaces:
//
//   QUALITY_LEVEL = 0u → 9-tap kernel  (sigma ≈ 2.0)
//                 = 1u → 17-tap kernel (sigma ≈ 4.0, default)
//                 = 2u → 25-tap kernel (sigma ≈ 6.0)
//
//   WEIGHTING_MODE = 0u → plain Gaussian (default; legacy behaviour)
//                  = 1u → scatter-as-gather by CoC: each neighbor only
//                         contributes if its CoC ≥ the center's, or if
//                         the center is itself very blurry. Prevents
//                         sharp-foreground bleed into blur regions —
//                         load-bearing for DoF-class CoC-driven blurs.
//
// Adapted verbatim from `effects/shaders/fx_depth_of_field_compute.wgsl`'s
// blur_*tap routines.
//
// see gaussian_blur_variable_width_body.wgsl's
// header for the full rationale — each logical tap now densifies into
// `subtap_count(step_size)` sub-samples that fill the gap back toward the
// previous tap when spacing exceeds GAP_THRESHOLD_PX, so a large per-pixel
// CoC no longer leaves visible ring gaps. `subtaps == 1` (small/typical
// radius, including the max_radius = 6.0 DoF-parity setting) reduces to the
// original single-sample-per-tap arithmetic exactly.

struct BlurUniforms {
    direction: u32,       // 0 = horizontal, 1 = vertical
    max_radius: f32,      // hard cap on width sample × max_radius
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: BlurUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var t_width: texture_2d<f32>;
@group(0) @binding(3) var s_source: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// 9-tap (sigma ≈ 2.0)
const K9: array<f32, 5> = array<f32, 5>(
    0.16501, 0.15019, 0.11325, 0.07076, 0.03664,
);

// 17-tap (sigma ≈ 4.0)
const K17: array<f32, 9> = array<f32, 9>(
    0.10315, 0.09998, 0.09103, 0.07786, 0.06257,
    0.04723, 0.03350, 0.02232, 0.01396,
);

// 25-tap (sigma ≈ 6.0)
const K25: array<f32, 13> = array<f32, 13>(
    0.07087, 0.06947, 0.06540, 0.05917, 0.05148,
    0.04307, 0.03465, 0.02680, 0.01995, 0.01428,
    0.00983, 0.00651, 0.00415,
);

const GAP_THRESHOLD_PX: f32 = 8.0;
const SUBTAP_CAP: i32 = 4;

fn subtap_count(step_size: f32) -> i32 {
    let raw = i32(ceil(step_size / GAP_THRESHOLD_PX));
    return clamp(raw, 1, SUBTAP_CAP);
}

// Scatter-as-gather CoC weight gate. WEIGHTING_MODE = 0 returns 1.0
// (every neighbor contributes); WEIGHTING_MODE = 1 returns the legacy
// DoF gate: contribution allowed only when the neighbor's CoC is at
// least the center's, OR the center is itself very blurry (CoC > 0.5).
fn coc_weight(center_coc: f32, neighbor_coc: f32) -> f32 {
    if WEIGHTING_MODE == 0u {
        return 1.0;
    }
    return select(
        step(center_coc, neighbor_coc),
        1.0,
        center_coc > 0.5,
    );
}

fn sample_tap(uv: vec2<f32>, d: vec2<f32>, sgn: f32, center_coc: f32) -> vec4<f32> {
    let p = uv + d * sgn;
    let rgb = textureSampleLevel(t_source, s_source, p, 0.0).rgb;
    let neighbor_coc = textureSampleLevel(t_width, s_source, p, 0.0).r;
    let w = coc_weight(center_coc, neighbor_coc);
    return vec4<f32>(rgb * w, w);
}

// Densifies logical tap `i` (weight `weight`) into `subtaps` evenly-spaced
// sub-samples filling the segment back toward tap `i - 1`, each carrying
// `weight / subtaps`. `subtaps == 1` collapses to the original single
// sample per tap per side.
fn tap_group(uv: vec2<f32>, d: vec2<f32>, i: i32, weight: f32, subtaps: i32, center_coc: f32) -> vec4<f32> {
    var acc = vec3<f32>(0.0, 0.0, 0.0);
    var w_acc = 0.0;
    let fw = weight / f32(subtaps);
    for (var k: i32 = 0; k < subtaps; k = k + 1) {
        let frac = f32(i) - f32(k) / f32(subtaps);
        var s = sample_tap(uv, d, frac, center_coc);
        acc += s.rgb * fw;
        w_acc += s.a * fw;
        s = sample_tap(uv, d, -frac, center_coc);
        acc += s.rgb * fw;
        w_acc += s.a * fw;
    }
    return vec4<f32>(acc, w_acc);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let center = textureSampleLevel(t_source, s_source, uv, 0.0);

    let center_coc = clamp(
        textureSampleLevel(t_width, s_source, uv, 0.0).r,
        0.0,
        1.0,
    );
    if center_coc < 0.005 {
        textureStore(output_tex, vec2<i32>(gid.xy), center);
        return;
    }

    let step_size = center_coc * u.max_radius + 1.0;
    let texel = 1.0 / vec2<f32>(dims);
    let dir = select(
        vec2<f32>(0.0, texel.y),
        vec2<f32>(texel.x, 0.0),
        u.direction == 0u,
    );
    let d = dir * step_size;
    let subtaps = subtap_count(step_size);

    var acc: vec3<f32>;
    var w_acc: f32;

    if QUALITY_LEVEL == 0u {
        // 9-tap
        acc = center.rgb * K9[0];
        w_acc = K9[0];
        for (var i: i32 = 1; i <= 4; i = i + 1) {
            let g = tap_group(uv, d, i, K9[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    } else if QUALITY_LEVEL == 2u {
        // 25-tap
        acc = center.rgb * K25[0];
        w_acc = K25[0];
        for (var i: i32 = 1; i <= 12; i = i + 1) {
            let g = tap_group(uv, d, i, K25[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    } else {
        // 17-tap (default)
        acc = center.rgb * K17[0];
        w_acc = K17[0];
        for (var i: i32 = 1; i <= 8; i = i + 1) {
            let g = tap_group(uv, d, i, K17[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    }

    let rgb = acc / max(w_acc, 0.001);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(rgb, center.a));
}
