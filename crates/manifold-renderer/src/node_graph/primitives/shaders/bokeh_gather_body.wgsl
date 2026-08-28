// node.bokeh_gather — fusable body (freeze section 12), 2-input GATHER via the
// STENCIL-FETCH ABI. Single-pass occlusion-aware disc gather DoF
// (docs/CINEMATIC_POST_DESIGN.md D5), replacing the two-pass separable
// node.variable_blur H/V gather inside CinematicScene (CINEMATIC_POST P4).
//
// MIP-GATHER UPGRADE (2026-08-28, silhouette-edge speckle fix): at full blur
// the 32-tap disc covers ~1800 px² (~1 sample per 56 px²), so each tap at a
// bright-on-black silhouette was a coin flip between a hot pixel and black,
// and the per-pixel spiral rotation decorrelated neighboring pixels'
// outcomes — sparse static dots hugging hard edges. The fix: `in` arrives
// bound as a MIPMAPPED prefiltered copy (run() builds the chain with exact
// box-average downsamples), and tap colors sample at a fractional LOD
// derived from the center pixel's CoC so the disc stays dense at the sampled
// level (~4-texel effective radius). The coin flip becomes an area average —
// deterministic and smooth across neighboring centers. CoC weights stay
// full-res (fetch_width at LOD 0): occlusion boundaries remain crisp, only
// color is variance-reduced.
//
// LOD FORMULA: lod = clamp(log2(center_coc_px / 4), 0, 8) — one LOD per
// pixel for the whole disc (per-tap LOD would leave the inner taps at LOD 0
// exactly where speckle lives, and break the weight normalization's energy
// conservation). Fractional LOD + trilinear sampling: the LOD field varies
// smoothly with CoC, so there are no banding transitions. Small CoC clamps
// to 0 = full res, no cost.
//
// This atom is fusion-EXEMPT (BoundaryReason::BarrieredReduction — the
// internal mip chain is a barriered multi-pass prefilter the fused form can
// never express). The body is only ever emitted STANDALONE (via
// standalone_for_boundary_spec), which is what makes the raw
// `textureSampleLevel(tex_in, samp, …, lod)` call below legal: `tex_in`/
// `samp` are the codegen's standalone binding names, and no fused emission
// exists to break. run() binds the mipmapped chain as `in` and a
// mip_filter=Linear sampler as `samp`.
//
// Exact algorithm, no substitution:
//
//   1. center_coc_frac = width(uv).r (coc_from_depth/coc_dilate's [0,1]
//      fraction-of-max_radius convention); center_coc_frac < 0.005 ->
//      pass-through (mirrors node.variable_blur's own in-focus early-out,
//      and is what makes I2 — a zero-CoC lens — bit-clean: level 0 of the
//      chain is an exact copy of `in`).
//   2. center_coc_px = center_coc_frac * max_radius;
//      lod = clamp(log2(center_coc_px / 4), 0, 8).
//   3. 32 golden-angle spiral taps (docs/CINEMATIC_POST_DESIGN.md D2:
//      r_i = sqrt((i+0.5)/32), theta_i = i*2.399963), rotated per-pixel by
//      D2's committed hash, scaled by center_coc_px — the disc radius is the
//      CENTER pixel's own CoC, not each tap's. Tap UVs are computed in
//      full-res pixel space; only the SAMPLING LEVEL changes.
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
// `width` is a Gather stencil-fetch input (`fetch_width`, defined by the
// codegen as a real textureSampleLevel over the bound texture) — full-res
// always. PARAMS: [max_radius, enabled]. `enabled` is host-only: the codegen
// path lays every param into the uniform struct, so the body accepts it but
// never reads it — skip_passthrough aliases `in`→`out` when
// `enabled = false`, so this body only runs when enabled.
// Matches bokeh_gather.wgsl (the hand parity oracle) — kept independent (not
// sharing source) so the gpu_tests parity check is a real cross-check.

const BOKEH_N: u32 = 32u;
const BOKEH_GOLDEN_ANGLE: f32 = 2.399963;
// Effective disc radius at the sampled mip level the LOD formula targets:
// 32 taps over a radius-4 disc is ~1.6 texels²/sample — dense enough that
// the gather stops undersampling the prefiltered signal.
const BOKEH_LOD_TARGET_RADIUS: f32 = 4.0;

// D2's committed per-pixel rotation hash (docs/CINEMATIC_POST_DESIGN.md D2) —
// same formula as ssao_from_depth_body.wgsl's ssao_hash_angle / film_grain's
// white_noise base, scaled to radians so it adds directly to theta_i.
fn bokeh_hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

fn body(uv: vec2<f32>, dims: vec2<f32>, max_radius: f32, enabled: u32) -> vec4<f32> {
    let center = fetch_in(uv);
    let center_coc_frac = clamp(fetch_width(uv).r, 0.0, 1.0);
    if center_coc_frac < 0.005 {
        return center;
    }

    let center_coc_px = center_coc_frac * max_radius;
    let lod = clamp(log2(center_coc_px / BOKEH_LOD_TARGET_RADIUS), 0.0, 8.0);
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

        // Mip-gather: area-averaged color at the CoC-proportional LOD.
        // `tex_in` is the mipmapped prefiltered chain bound by run();
        // textureSampleLevel clamps lod to the chain's depth, so the 8.0
        // ceiling above is a formality, not a requirement.
        let tap_color = textureSampleLevel(tex_in, samp, tap_uv, lod).rgb;
        let tap_coc_px = clamp(fetch_width(tap_uv).r, 0.0, 1.0) * max_radius;
        let distance_to_center_px = length(offset_px);
        let w = step(distance_to_center_px, tap_coc_px);

        acc = acc + tap_color * w;
        w_acc = w_acc + w;
    }

    let rgb = select(center.rgb, acc / max(w_acc, 0.0001), w_acc > 0.0);
    return vec4<f32>(rgb, center.a);
}
