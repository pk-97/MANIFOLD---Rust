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
const K9_0: f32 = 0.16501;
const K9_1: f32 = 0.15019;
const K9_2: f32 = 0.11325;
const K9_3: f32 = 0.07076;
const K9_4: f32 = 0.03664;

// 17-tap (sigma ≈ 4.0)
const K17_0: f32 = 0.10315;
const K17_1: f32 = 0.09998;
const K17_2: f32 = 0.09103;
const K17_3: f32 = 0.07786;
const K17_4: f32 = 0.06257;
const K17_5: f32 = 0.04723;
const K17_6: f32 = 0.03350;
const K17_7: f32 = 0.02232;
const K17_8: f32 = 0.01396;

// 25-tap (sigma ≈ 6.0)
const K25_0:  f32 = 0.07087;
const K25_1:  f32 = 0.06947;
const K25_2:  f32 = 0.06540;
const K25_3:  f32 = 0.05917;
const K25_4:  f32 = 0.05148;
const K25_5:  f32 = 0.04307;
const K25_6:  f32 = 0.03465;
const K25_7:  f32 = 0.02680;
const K25_8:  f32 = 0.01995;
const K25_9:  f32 = 0.01428;
const K25_10: f32 = 0.00983;
const K25_11: f32 = 0.00651;
const K25_12: f32 = 0.00415;

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

    var acc: vec3<f32>;
    var w_acc: f32;
    var s: vec4<f32>;

    if QUALITY_LEVEL == 0u {
        // 9-tap
        acc = center.rgb * K9_0;
        w_acc = K9_0;
        s = sample_tap(uv, d, 1.0, center_coc); acc += s.rgb * K9_1; w_acc += s.a * K9_1;
        s = sample_tap(uv, d, -1.0, center_coc); acc += s.rgb * K9_1; w_acc += s.a * K9_1;
        s = sample_tap(uv, d, 2.0, center_coc); acc += s.rgb * K9_2; w_acc += s.a * K9_2;
        s = sample_tap(uv, d, -2.0, center_coc); acc += s.rgb * K9_2; w_acc += s.a * K9_2;
        s = sample_tap(uv, d, 3.0, center_coc); acc += s.rgb * K9_3; w_acc += s.a * K9_3;
        s = sample_tap(uv, d, -3.0, center_coc); acc += s.rgb * K9_3; w_acc += s.a * K9_3;
        s = sample_tap(uv, d, 4.0, center_coc); acc += s.rgb * K9_4; w_acc += s.a * K9_4;
        s = sample_tap(uv, d, -4.0, center_coc); acc += s.rgb * K9_4; w_acc += s.a * K9_4;
    } else if QUALITY_LEVEL == 2u {
        // 25-tap
        acc = center.rgb * K25_0;
        w_acc = K25_0;
        s = sample_tap(uv, d, 1.0, center_coc); acc += s.rgb * K25_1; w_acc += s.a * K25_1;
        s = sample_tap(uv, d, -1.0, center_coc); acc += s.rgb * K25_1; w_acc += s.a * K25_1;
        s = sample_tap(uv, d, 2.0, center_coc); acc += s.rgb * K25_2; w_acc += s.a * K25_2;
        s = sample_tap(uv, d, -2.0, center_coc); acc += s.rgb * K25_2; w_acc += s.a * K25_2;
        s = sample_tap(uv, d, 3.0, center_coc); acc += s.rgb * K25_3; w_acc += s.a * K25_3;
        s = sample_tap(uv, d, -3.0, center_coc); acc += s.rgb * K25_3; w_acc += s.a * K25_3;
        s = sample_tap(uv, d, 4.0, center_coc); acc += s.rgb * K25_4; w_acc += s.a * K25_4;
        s = sample_tap(uv, d, -4.0, center_coc); acc += s.rgb * K25_4; w_acc += s.a * K25_4;
        s = sample_tap(uv, d, 5.0, center_coc); acc += s.rgb * K25_5; w_acc += s.a * K25_5;
        s = sample_tap(uv, d, -5.0, center_coc); acc += s.rgb * K25_5; w_acc += s.a * K25_5;
        s = sample_tap(uv, d, 6.0, center_coc); acc += s.rgb * K25_6; w_acc += s.a * K25_6;
        s = sample_tap(uv, d, -6.0, center_coc); acc += s.rgb * K25_6; w_acc += s.a * K25_6;
        s = sample_tap(uv, d, 7.0, center_coc); acc += s.rgb * K25_7; w_acc += s.a * K25_7;
        s = sample_tap(uv, d, -7.0, center_coc); acc += s.rgb * K25_7; w_acc += s.a * K25_7;
        s = sample_tap(uv, d, 8.0, center_coc); acc += s.rgb * K25_8; w_acc += s.a * K25_8;
        s = sample_tap(uv, d, -8.0, center_coc); acc += s.rgb * K25_8; w_acc += s.a * K25_8;
        s = sample_tap(uv, d, 9.0, center_coc); acc += s.rgb * K25_9; w_acc += s.a * K25_9;
        s = sample_tap(uv, d, -9.0, center_coc); acc += s.rgb * K25_9; w_acc += s.a * K25_9;
        s = sample_tap(uv, d, 10.0, center_coc); acc += s.rgb * K25_10; w_acc += s.a * K25_10;
        s = sample_tap(uv, d, -10.0, center_coc); acc += s.rgb * K25_10; w_acc += s.a * K25_10;
        s = sample_tap(uv, d, 11.0, center_coc); acc += s.rgb * K25_11; w_acc += s.a * K25_11;
        s = sample_tap(uv, d, -11.0, center_coc); acc += s.rgb * K25_11; w_acc += s.a * K25_11;
        s = sample_tap(uv, d, 12.0, center_coc); acc += s.rgb * K25_12; w_acc += s.a * K25_12;
        s = sample_tap(uv, d, -12.0, center_coc); acc += s.rgb * K25_12; w_acc += s.a * K25_12;
    } else {
        // 17-tap (default)
        acc = center.rgb * K17_0;
        w_acc = K17_0;
        s = sample_tap(uv, d, 1.0, center_coc); acc += s.rgb * K17_1; w_acc += s.a * K17_1;
        s = sample_tap(uv, d, -1.0, center_coc); acc += s.rgb * K17_1; w_acc += s.a * K17_1;
        s = sample_tap(uv, d, 2.0, center_coc); acc += s.rgb * K17_2; w_acc += s.a * K17_2;
        s = sample_tap(uv, d, -2.0, center_coc); acc += s.rgb * K17_2; w_acc += s.a * K17_2;
        s = sample_tap(uv, d, 3.0, center_coc); acc += s.rgb * K17_3; w_acc += s.a * K17_3;
        s = sample_tap(uv, d, -3.0, center_coc); acc += s.rgb * K17_3; w_acc += s.a * K17_3;
        s = sample_tap(uv, d, 4.0, center_coc); acc += s.rgb * K17_4; w_acc += s.a * K17_4;
        s = sample_tap(uv, d, -4.0, center_coc); acc += s.rgb * K17_4; w_acc += s.a * K17_4;
        s = sample_tap(uv, d, 5.0, center_coc); acc += s.rgb * K17_5; w_acc += s.a * K17_5;
        s = sample_tap(uv, d, -5.0, center_coc); acc += s.rgb * K17_5; w_acc += s.a * K17_5;
        s = sample_tap(uv, d, 6.0, center_coc); acc += s.rgb * K17_6; w_acc += s.a * K17_6;
        s = sample_tap(uv, d, -6.0, center_coc); acc += s.rgb * K17_6; w_acc += s.a * K17_6;
        s = sample_tap(uv, d, 7.0, center_coc); acc += s.rgb * K17_7; w_acc += s.a * K17_7;
        s = sample_tap(uv, d, -7.0, center_coc); acc += s.rgb * K17_7; w_acc += s.a * K17_7;
        s = sample_tap(uv, d, 8.0, center_coc); acc += s.rgb * K17_8; w_acc += s.a * K17_8;
        s = sample_tap(uv, d, -8.0, center_coc); acc += s.rgb * K17_8; w_acc += s.a * K17_8;
    }

    let rgb = acc / max(w_acc, 0.001);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(rgb, center.a));
}
