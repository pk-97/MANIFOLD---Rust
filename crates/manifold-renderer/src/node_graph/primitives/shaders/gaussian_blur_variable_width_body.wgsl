// node.gaussian_blur_variable_width — fusable body (freeze §12), 2-input GATHER
// via the STENCIL-FETCH ABI, with SPECIALIZATION constants. Separable Gaussian
// blur whose per-pixel width is sampled from a `width` texture (R = CoC). Both
// `in` and `width` are gathered along one axis at body-computed tap offsets
// (texel = 1/dims), read through `fetch_in(uv)` / `fetch_width(uv)` — defined by
// the codegen as real samples (standalone / fused real externals) or recomputed
// virtual sources (fused). QUALITY_LEVEL (9/17/25-tap) and WEIGHTING_MODE
// (plain / scatter-as-gather-by-CoC) are PIPELINE-SPECIALIZATION tokens — run()
// substitutes them at compile via create_specialized_compute_pipeline; the
// freeze compiler substitutes the def's static `quality`/`weighting_mode` param
// values into the body text (wgsl_specialization) so the atom can fuse. Those
// PARAMS still arrive in the body signature (the codegen passes every param)
// but are UNUSED here. Matches gaussian_blur_variable_width.wgsl. PARAMS:
// [axis, max_radius, quality (Enum->u32, specialization), weighting_mode
// (Enum->u32, specialization)].
//
// BUG-138 fix (2026-07-13): the original kernel stepped every logical tap by
// the SAME `step_size` (derived from the center pixel's CoC), so a large CoC
// spread the fixed 9/17/25 taps across a wide span with big gaps between the
// actual samples — visible rings instead of a smooth falloff. Fix shape per
// the bug's own note ("scale tap count with radius"): each logical tap now
// densifies into `vbw_subtap_count(step_size)` sub-samples that fill the gap
// back toward the previous tap, weight split evenly across the sub-samples.
// At `step_size <= VBW_GAP_THRESHOLD_PX` (covers max_radius=6 DoF parity —
// see composition_notes) `subtaps == 1` and this reduces to exactly the
// original single-sample-per-tap arithmetic (byte-identical), so the
// documented "matches legacy DoF blur byte-for-byte" parity claim still
// holds. `VBW_SUBTAP_CAP` bounds worst-case per-pixel cost (4x at most,
// deliberately — see docs/BUG_BACKLOG.md BUG-138 for the perf/quality
// tradeoff this cap represents).

// 9-tap (sigma ~= 2.0)
const VBW_K9: array<f32, 5> = array<f32, 5>(
    0.16501, 0.15019, 0.11325, 0.07076, 0.03664,
);

// 17-tap (sigma ~= 4.0)
const VBW_K17: array<f32, 9> = array<f32, 9>(
    0.10315, 0.09998, 0.09103, 0.07786, 0.06257,
    0.04723, 0.03350, 0.02232, 0.01396,
);

// 25-tap (sigma ~= 6.0)
const VBW_K25: array<f32, 13> = array<f32, 13>(
    0.07087, 0.06947, 0.06540, 0.05917, 0.05148,
    0.04307, 0.03465, 0.02680, 0.01995, 0.01428,
    0.00983, 0.00651, 0.00415,
);

// BUG-138: tap-count-scales-with-radius fallback. Below this per-tap spacing
// (px) the kernel is unchanged (subtaps == 1); at max_radius = 6.0 (DoF
// parity, composition_notes) step_size is at most 7.0 px, safely under this
// threshold. SUBTAP_CAP bounds the worst-case per-pixel tap-multiplier.
const VBW_GAP_THRESHOLD_PX: f32 = 8.0;
const VBW_SUBTAP_CAP: i32 = 4;

fn vbw_subtap_count(step_size: f32) -> i32 {
    let raw = i32(ceil(step_size / VBW_GAP_THRESHOLD_PX));
    return clamp(raw, 1, VBW_SUBTAP_CAP);
}

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

fn vbw_sample_tap(uv: vec2<f32>, d: vec2<f32>, sgn: f32, center_coc: f32) -> vec4<f32> {
    let p = uv + d * sgn;
    let rgb = fetch_in(p).rgb;
    let neighbor_coc = fetch_width(p).r;
    let w = coc_weight(center_coc, neighbor_coc);
    return vec4<f32>(rgb * w, w);
}

// Densifies logical tap `i` (weight `weight`) into `subtaps` evenly-spaced
// sub-samples filling the segment back toward tap `i - 1`, each carrying
// `weight / subtaps`. `subtaps == 1` collapses to exactly the original
// single-sample-per-tap-per-side arithmetic.
fn vbw_tap_group(uv: vec2<f32>, d: vec2<f32>, i: i32, weight: f32, subtaps: i32, center_coc: f32) -> vec4<f32> {
    var acc = vec3<f32>(0.0, 0.0, 0.0);
    var w_acc = 0.0;
    let fw = weight / f32(subtaps);
    for (var k: i32 = 0; k < subtaps; k = k + 1) {
        let frac = f32(i) - f32(k) / f32(subtaps);
        var s = vbw_sample_tap(uv, d, frac, center_coc);
        acc += s.rgb * fw;
        w_acc += s.a * fw;
        s = vbw_sample_tap(uv, d, -frac, center_coc);
        acc += s.rgb * fw;
        w_acc += s.a * fw;
    }
    return vec4<f32>(acc, w_acc);
}

fn body(uv: vec2<f32>, dims: vec2<f32>, direction: u32, max_radius: f32, quality: u32, weighting_mode: u32) -> vec4<f32> {
    let center = fetch_in(uv);

    let center_coc = clamp(fetch_width(uv).r, 0.0, 1.0);
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
    let subtaps = vbw_subtap_count(step_size);

    var acc: vec3<f32>;
    var w_acc: f32;

    if QUALITY_LEVEL == 0u {
        // 9-tap
        acc = center.rgb * VBW_K9[0];
        w_acc = VBW_K9[0];
        for (var i: i32 = 1; i <= 4; i = i + 1) {
            let g = vbw_tap_group(uv, d, i, VBW_K9[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    } else if QUALITY_LEVEL == 2u {
        // 25-tap
        acc = center.rgb * VBW_K25[0];
        w_acc = VBW_K25[0];
        for (var i: i32 = 1; i <= 12; i = i + 1) {
            let g = vbw_tap_group(uv, d, i, VBW_K25[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    } else {
        // 17-tap (default)
        acc = center.rgb * VBW_K17[0];
        w_acc = VBW_K17[0];
        for (var i: i32 = 1; i <= 8; i = i + 1) {
            let g = vbw_tap_group(uv, d, i, VBW_K17[i], subtaps, center_coc);
            acc += g.rgb;
            w_acc += g.a;
        }
    }

    let rgb = acc / max(w_acc, 0.001);
    return vec4<f32>(rgb, center.a);
}
