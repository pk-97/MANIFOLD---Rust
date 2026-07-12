// node.motion_blur — fusable body (freeze §12), MultiInputCoincident:
// `in` is Gather (stencil-fetch), `velocity` is CoincidentTexel.
//
// Velocity-directed gather motion blur (docs/CINEMATIC_POST_DESIGN.md D4).
// Exact formula, no substitution:
//   smear_px = velocity_ndc * 0.5 * dims * (shutter_angle / 360.0), clamped
//              component-wise to [-max_blur_px, max_blur_px]
//   8 equal-weight taps of `in`, evenly spaced INCLUSIVE of both endpoints
//   from uv - smear_uv/2 to uv + smear_uv/2:
//     t_i = i / (N - 1) - 0.5,  i in 0..N,  N = MOTION_BLUR_SAMPLES = 8
//   (D4: "samples fixed 8 (const)" — a WGSL const, never a runtime param).
//   out = average of the 8 taps (rgb AND alpha, equal weights — no
//   CoC-style center-alpha preservation; that's node.variable_blur's
//   DIFFERENT per-tap weighting scheme for a different reason, anti-bleed
//   at silhouette edges. D4 names no such exception here, and averaging
//   alpha uniformly alongside rgb is what keeps this exact at shutter=0:
//   every tap then collapses onto the SAME texel, so the average equals
//   that texel's full RGBA unchanged — I2).
//
// `velocity` is CoincidentTexel (own-texel, exact integer load, no
// filtering) — a directional vector must never be blended with a
// neighbour's, which filtering would do. `in` is Gather: the tap
// coordinate is body-computed (uv + t*smear_uv), so the codegen can't
// pre-sample it into a register; `fetch_in(uv)` is the free stencil-fetch
// function the codegen emits for a Gather input (real `textureSampleLevel`
// under the hood), which additionally lets an upstream fusable pointwise
// producer virtually chain into this dispatch instead of materializing
// first (same mechanism node.variable_blur's `fetch_in`/`fetch_width`
// use).
//
// Sign-convention note (verified by derivation, not by inspection):
// `velocity`'s NDC delta is `ndc_now - ndc_prev` in clip space (WGSL
// clip-space y-up); this atom's uv/texel space is texture-convention
// y-down. The two disagree on the sign of the y component, but taps are
// placed SYMMETRICALLY at +/-smear_uv/2 around `uv` — negating either
// axis of smear_uv only relabels which physical direction is "positive"
// within an already-symmetric sample set, so the accumulated average is
// provably invariant to the flip. No sign correction is applied.
//
// `camera` reads `lens.shutter_angle` ENTIRELY via the one DERIVED_UNIFORMS
// field below (written upstream by node.camera_lens — "one lens, every
// consumer reads it", docs/CAMERA_AND_LENS_DESIGN.md D4) — it never becomes
// a GPU binding. `in`'s Gather access still makes this atom a permanent
// fusion Boundary going forward (a gather can never fuse with its OWN
// producer — D7's honest-scope note), so this doesn't buy fusion with a
// downstream consumer; it buys the upstream producer chain fusing INTO
// this dispatch via stencil-fetch, and keeps this atom on the single
// generated-kernel codegen path (no hand-rolled runtime kernel).
//
// PARAMS: [max_blur_px]. DERIVED_UNIFORMS: [shutter_angle]. Matches
// motion_blur.wgsl (the hand parity oracle).

const MOTION_BLUR_SAMPLES: u32 = 8u;

fn body(
    c_velocity: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    max_blur_px: f32,
    shutter_angle: f32,
) -> vec4<f32> {
    let velocity_ndc = c_velocity.rg;
    let smear_px_raw = velocity_ndc * 0.5 * dims * (shutter_angle / 360.0);
    let smear_px = clamp(smear_px_raw, vec2<f32>(-max_blur_px), vec2<f32>(max_blur_px));
    let smear_uv = smear_px / dims;

    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var i: u32 = 0u; i < MOTION_BLUR_SAMPLES; i = i + 1u) {
        let t = f32(i) / f32(MOTION_BLUR_SAMPLES - 1u) - 0.5;
        acc = acc + fetch_in(uv + smear_uv * t);
    }
    return acc / f32(MOTION_BLUR_SAMPLES);
}
