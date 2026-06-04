// node.gaussian_blur_variable_width — fusable body (freeze §12), 2-input GATHER
// with SPECIALIZATION constants. Separable Gaussian blur whose per-pixel width is
// sampled from a `width` texture (R = CoC). Both `in` and `width` are gathered
// along one axis at body-computed tap offsets (texel = 1/dims). QUALITY_LEVEL
// (9/17/25-tap) and WEIGHTING_MODE (plain / scatter-as-gather-by-CoC) are
// PIPELINE-SPECIALIZATION tokens — run() substitutes them at compile via
// create_specialized_compute_pipeline, so the dead tap branches flatten away and
// the per-variant perf is preserved. The `quality`/`weighting_mode` PARAMS still
// arrive in the body signature (the codegen passes every param) but are UNUSED
// here; run() reads them to pick the specialization key. Matches
// gaussian_blur_variable_width.wgsl. PARAMS: [axis, max_radius, quality
// (Enum->u32, specialization), weighting_mode (Enum->u32, specialization)].

// 9-tap (sigma ~= 2.0)
const VBW_K9_0: f32 = 0.16501;
const VBW_K9_1: f32 = 0.15019;
const VBW_K9_2: f32 = 0.11325;
const VBW_K9_3: f32 = 0.07076;
const VBW_K9_4: f32 = 0.03664;

// 17-tap (sigma ~= 4.0)
const VBW_K17_0: f32 = 0.10315;
const VBW_K17_1: f32 = 0.09998;
const VBW_K17_2: f32 = 0.09103;
const VBW_K17_3: f32 = 0.07786;
const VBW_K17_4: f32 = 0.06257;
const VBW_K17_5: f32 = 0.04723;
const VBW_K17_6: f32 = 0.03350;
const VBW_K17_7: f32 = 0.02232;
const VBW_K17_8: f32 = 0.01396;

// 25-tap (sigma ~= 6.0)
const VBW_K25_0:  f32 = 0.07087;
const VBW_K25_1:  f32 = 0.06947;
const VBW_K25_2:  f32 = 0.06540;
const VBW_K25_3:  f32 = 0.05917;
const VBW_K25_4:  f32 = 0.05148;
const VBW_K25_5:  f32 = 0.04307;
const VBW_K25_6:  f32 = 0.03465;
const VBW_K25_7:  f32 = 0.02680;
const VBW_K25_8:  f32 = 0.01995;
const VBW_K25_9:  f32 = 0.01428;
const VBW_K25_10: f32 = 0.00983;
const VBW_K25_11: f32 = 0.00651;
const VBW_K25_12: f32 = 0.00415;

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

fn vbw_sample_tap(t_source: texture_2d<f32>, s_source: sampler, t_width: texture_2d<f32>, s_width: sampler, uv: vec2<f32>, d: vec2<f32>, sgn: f32, center_coc: f32) -> vec4<f32> {
    let p = uv + d * sgn;
    let rgb = textureSampleLevel(t_source, s_source, p, 0.0).rgb;
    let neighbor_coc = textureSampleLevel(t_width, s_width, p, 0.0).r;
    let w = coc_weight(center_coc, neighbor_coc);
    return vec4<f32>(rgb * w, w);
}

fn body(t_source: texture_2d<f32>, s_source: sampler, t_width: texture_2d<f32>, s_width: sampler, uv: vec2<f32>, dims: vec2<f32>, direction: u32, max_radius: f32, quality: u32, weighting_mode: u32) -> vec4<f32> {
    let center = textureSampleLevel(t_source, s_source, uv, 0.0);

    let center_coc = clamp(textureSampleLevel(t_width, s_width, uv, 0.0).r, 0.0, 1.0);
    if center_coc < 0.005 {
        return center;
    }

    let step_size = center_coc * max_radius + 1.0;
    let texel = 1.0 / dims;
    let dir = select(
        vec2<f32>(0.0, texel.y),
        vec2<f32>(texel.x, 0.0),
        direction == 0u,
    );
    let d = dir * step_size;

    var acc: vec3<f32>;
    var w_acc: f32;
    var s: vec4<f32>;

    if QUALITY_LEVEL == 0u {
        // 9-tap
        acc = center.rgb * VBW_K9_0;
        w_acc = VBW_K9_0;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 1.0, center_coc); acc += s.rgb * VBW_K9_1; w_acc += s.a * VBW_K9_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -1.0, center_coc); acc += s.rgb * VBW_K9_1; w_acc += s.a * VBW_K9_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 2.0, center_coc); acc += s.rgb * VBW_K9_2; w_acc += s.a * VBW_K9_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -2.0, center_coc); acc += s.rgb * VBW_K9_2; w_acc += s.a * VBW_K9_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 3.0, center_coc); acc += s.rgb * VBW_K9_3; w_acc += s.a * VBW_K9_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -3.0, center_coc); acc += s.rgb * VBW_K9_3; w_acc += s.a * VBW_K9_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 4.0, center_coc); acc += s.rgb * VBW_K9_4; w_acc += s.a * VBW_K9_4;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -4.0, center_coc); acc += s.rgb * VBW_K9_4; w_acc += s.a * VBW_K9_4;
    } else if QUALITY_LEVEL == 2u {
        // 25-tap
        acc = center.rgb * VBW_K25_0;
        w_acc = VBW_K25_0;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 1.0, center_coc); acc += s.rgb * VBW_K25_1; w_acc += s.a * VBW_K25_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -1.0, center_coc); acc += s.rgb * VBW_K25_1; w_acc += s.a * VBW_K25_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 2.0, center_coc); acc += s.rgb * VBW_K25_2; w_acc += s.a * VBW_K25_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -2.0, center_coc); acc += s.rgb * VBW_K25_2; w_acc += s.a * VBW_K25_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 3.0, center_coc); acc += s.rgb * VBW_K25_3; w_acc += s.a * VBW_K25_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -3.0, center_coc); acc += s.rgb * VBW_K25_3; w_acc += s.a * VBW_K25_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 4.0, center_coc); acc += s.rgb * VBW_K25_4; w_acc += s.a * VBW_K25_4;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -4.0, center_coc); acc += s.rgb * VBW_K25_4; w_acc += s.a * VBW_K25_4;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 5.0, center_coc); acc += s.rgb * VBW_K25_5; w_acc += s.a * VBW_K25_5;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -5.0, center_coc); acc += s.rgb * VBW_K25_5; w_acc += s.a * VBW_K25_5;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 6.0, center_coc); acc += s.rgb * VBW_K25_6; w_acc += s.a * VBW_K25_6;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -6.0, center_coc); acc += s.rgb * VBW_K25_6; w_acc += s.a * VBW_K25_6;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 7.0, center_coc); acc += s.rgb * VBW_K25_7; w_acc += s.a * VBW_K25_7;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -7.0, center_coc); acc += s.rgb * VBW_K25_7; w_acc += s.a * VBW_K25_7;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 8.0, center_coc); acc += s.rgb * VBW_K25_8; w_acc += s.a * VBW_K25_8;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -8.0, center_coc); acc += s.rgb * VBW_K25_8; w_acc += s.a * VBW_K25_8;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 9.0, center_coc); acc += s.rgb * VBW_K25_9; w_acc += s.a * VBW_K25_9;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -9.0, center_coc); acc += s.rgb * VBW_K25_9; w_acc += s.a * VBW_K25_9;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 10.0, center_coc); acc += s.rgb * VBW_K25_10; w_acc += s.a * VBW_K25_10;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -10.0, center_coc); acc += s.rgb * VBW_K25_10; w_acc += s.a * VBW_K25_10;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 11.0, center_coc); acc += s.rgb * VBW_K25_11; w_acc += s.a * VBW_K25_11;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -11.0, center_coc); acc += s.rgb * VBW_K25_11; w_acc += s.a * VBW_K25_11;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 12.0, center_coc); acc += s.rgb * VBW_K25_12; w_acc += s.a * VBW_K25_12;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -12.0, center_coc); acc += s.rgb * VBW_K25_12; w_acc += s.a * VBW_K25_12;
    } else {
        // 17-tap (default)
        acc = center.rgb * VBW_K17_0;
        w_acc = VBW_K17_0;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 1.0, center_coc); acc += s.rgb * VBW_K17_1; w_acc += s.a * VBW_K17_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -1.0, center_coc); acc += s.rgb * VBW_K17_1; w_acc += s.a * VBW_K17_1;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 2.0, center_coc); acc += s.rgb * VBW_K17_2; w_acc += s.a * VBW_K17_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -2.0, center_coc); acc += s.rgb * VBW_K17_2; w_acc += s.a * VBW_K17_2;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 3.0, center_coc); acc += s.rgb * VBW_K17_3; w_acc += s.a * VBW_K17_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -3.0, center_coc); acc += s.rgb * VBW_K17_3; w_acc += s.a * VBW_K17_3;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 4.0, center_coc); acc += s.rgb * VBW_K17_4; w_acc += s.a * VBW_K17_4;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -4.0, center_coc); acc += s.rgb * VBW_K17_4; w_acc += s.a * VBW_K17_4;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 5.0, center_coc); acc += s.rgb * VBW_K17_5; w_acc += s.a * VBW_K17_5;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -5.0, center_coc); acc += s.rgb * VBW_K17_5; w_acc += s.a * VBW_K17_5;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 6.0, center_coc); acc += s.rgb * VBW_K17_6; w_acc += s.a * VBW_K17_6;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -6.0, center_coc); acc += s.rgb * VBW_K17_6; w_acc += s.a * VBW_K17_6;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 7.0, center_coc); acc += s.rgb * VBW_K17_7; w_acc += s.a * VBW_K17_7;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -7.0, center_coc); acc += s.rgb * VBW_K17_7; w_acc += s.a * VBW_K17_7;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, 8.0, center_coc); acc += s.rgb * VBW_K17_8; w_acc += s.a * VBW_K17_8;
        s = vbw_sample_tap(t_source, s_source, t_width, s_width, uv, d, -8.0, center_coc); acc += s.rgb * VBW_K17_8; w_acc += s.a * VBW_K17_8;
    }

    let rgb = acc / max(w_acc, 0.001);
    return vec4<f32>(rgb, center.a);
}
