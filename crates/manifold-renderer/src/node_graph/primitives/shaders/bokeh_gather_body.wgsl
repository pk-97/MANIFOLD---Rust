// node.bokeh_gather — fusable body (freeze §12), 2-input GATHER via the
// STENCIL-FETCH ABI (matches node.variable_blur's shape). Single-pass
// occlusion-aware disc gather DoF (docs/CINEMATIC_POST_DESIGN.md D5),
// replacing the two-pass separable node.variable_blur H/V gather inside
// CinematicScene (CINEMATIC_POST P4). Exact algorithm, no substitution:
//
//   1. center_coc_frac = width(uv).r (coc_from_depth/coc_dilate's [0,1]
//      fraction-of-max_radius convention); center_coc_frac < 0.005 ->
//      pass-through (mirrors node.variable_blur's own in-focus early-out,
//      and is what makes I2 — a zero-CoC lens — bit-clean).
//   2. center_coc_px = center_coc_frac * max_radius.
//   3. 32 golden-angle spiral taps (docs/CINEMATIC_POST_DESIGN.md D2:
//      r_i = sqrt((i+0.5)/32), theta_i = i*2.399963), rotated per-pixel by
//      D2's committed hash, scaled by center_coc_px — the disc radius is the
//      CENTER pixel's own CoC, not each tap's.
//   4. Each tap's own CoC (sampled fresh from `width` at the tap's UV,
//      scaled to px the same way) sets whether it contributes:
//      weight = step(distance_to_center_px, tap_coc_px) — a sample only
//      contributes if its own CoC reaches (or exceeds) the distance back to
//      the center (the standard scatter-as-gather occlusion approximation
//      named in D5; same shape as node.variable_blur's ScatterAsGatherByCoC
//      weighting_mode, generalized from 1D taps to a 2D disc).
//   5. Luminance-preserving normalization: divide the accumulated color by
//      the accumulated weight; if the weight sum is exactly 0 (every tap
//      occluded), fall back to the center color instead of dividing by
//      zero. Circular aperture v1 — no blade-count shaping.
//
// `in`/`width` are both Gather stencil-fetch inputs (`fetch_in`/`fetch_width`,
// defined by the codegen as real samples for standalone/fused-real-external,
// or recomputed upstream chains for fused-virtual-source) — the tap UV is
// body-computed per-iteration from the spiral+CoC math, so neither input can
// be pre-sampled into a register by the codegen. PARAMS: [max_radius].
// Matches bokeh_gather.wgsl (the hand parity oracle) — kept independent (not
// sharing source) so the gpu_tests parity check is a real cross-check.

const BOKEH_N: u32 = 32u;
const BOKEH_GOLDEN_ANGLE: f32 = 2.399963;

// D2's committed per-pixel rotation hash (docs/CINEMATIC_POST_DESIGN.md D2) —
// same formula as ssao_from_depth_body.wgsl's ssao_hash_angle / film_grain's
// white_noise base, scaled to radians so it adds directly to theta_i.
fn bokeh_hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

fn body(uv: vec2<f32>, dims: vec2<f32>, max_radius: f32) -> vec4<f32> {
    let center = fetch_in(uv);
    let center_coc_frac = clamp(fetch_width(uv).r, 0.0, 1.0);
    if center_coc_frac < 0.005 {
        return center;
    }

    let center_coc_px = center_coc_frac * max_radius;
    let texel = 1.0 / dims;
    let px = uv * dims;
    let rot = bokeh_hash_angle(px);

    var acc: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var w_acc: f32 = 0.0;

    for (var i: u32 = 0u; i < BOKEH_N; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(BOKEH_N));
        let theta = f32(i) * BOKEH_GOLDEN_ANGLE + rot;
        let offset_px = vec2<f32>(r * cos(theta), r * sin(theta)) * center_coc_px;
        let tap_uv = uv + offset_px * texel;

        let tap_color = fetch_in(tap_uv).rgb;
        let tap_coc_px = clamp(fetch_width(tap_uv).r, 0.0, 1.0) * max_radius;
        let distance_to_center_px = length(offset_px);
        let w = step(distance_to_center_px, tap_coc_px);

        acc = acc + tap_color * w;
        w_acc = w_acc + w;
    }

    let rgb = select(center.rgb, acc / max(w_acc, 0.0001), w_acc > 0.0);
    return vec4<f32>(rgb, center.a);
}
