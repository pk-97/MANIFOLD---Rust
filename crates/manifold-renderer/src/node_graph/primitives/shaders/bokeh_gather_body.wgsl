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
//      scaled to px the same way) sets how much it contributes:
//      weight = clamp((tap_coc_px - distance + RAMP) / (2*RAMP), 0, 1) —
//      a sample counts in proportion to how far its own CoC reaches past
//      the distance back to the center (the standard scatter-as-gather
//      occlusion approximation named in D5, softened from D5's binary step
//      to a 2px ramp 2026-08-28: the binary cutoff + small included counts
//      + normalization was the residual spray-noise amplifier).
//   5. Coverage-filled normalization (2026-08-28, replaces D5's
//      divide-by-included-weight): out = acc/BOKEH_N + center *
//      (1 - w_acc/BOKEH_N) * focus_fill, where focus_fill =
//      1 - smoothstep(0, 0.25, center_coc_frac). The excluded taps' share
//      of the kernel is filled with the CENTER pixel's own color, so a
//      blurred halo dilutes smoothly into whatever is behind it (in the
//      black void: feathers to black — no plateau-then-cliff rim). The
//      focus gate confines the fill to SHARP pixels: a sharp foreground
//      interior fills with its own color (no dark fringe), while a
//      defocused texel has no unscattered remainder and scatters fully
//      (ungated, a hot texel kept a bright core — I3 regression).
//      D5's w_acc normalization held every blurred region at full
//      brightness right up to a hard cutoff — the "disconnected halo"
//      artifact Peter flagged on the music-video repro.
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
// 2 texels, so each tap's footprint (~1 texel) OVERLAPS its neighbors —
// taps read correlated area averages and the 32-tap spiral pattern fills
// rather than spraying. (Was 4: footprints were a third the size of their
// gaps and a small source mirrored back as 32 separated blobs.)
const BOKEH_LOD_TARGET_RADIUS: f32 = 2.0;
// Soft inclusion ramp width (px, full-res): a tap's weight fades 0→1 across
// [tap_coc - RAMP, tap_coc + RAMP] instead of flipping binary at the
// threshold. The binary step + small included-tap count + luminance
// normalization was the noise amplifier: one tap flipping changed the
// output by 1/w_acc, and the per-pixel hash decorrelated the flip between
// neighbors. Centered on the old threshold so occlusion reach is unchanged
// on average.
const BOKEH_INCLUSION_RAMP: f32 = 1.0;

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
        let w = clamp((tap_coc_px - distance_to_center_px + BOKEH_INCLUSION_RAMP)
                      / (2.0 * BOKEH_INCLUSION_RAMP), 0.0, 1.0);

        acc = acc + tap_color * w;
        w_acc = w_acc + w;
    }

    let coverage = w_acc / f32(BOKEH_N);
    // The fill is for SHARP pixels only (a foreground interior whose blurry
    // background taps were excluded fills with its own color — no dark
    // fringe). A defocused center has no unscattered remainder — gating by
    // the center's own CoC keeps a hot texel from retaining a bright core
    // (I3 regression).
    let focus_fill = 1.0 - smoothstep(0.0, 0.25, center_coc_frac);
    let rgb = acc / f32(BOKEH_N) + center.rgb * (1.0 - coverage) * focus_fill;
    return vec4<f32>(rgb, center.a);
}
