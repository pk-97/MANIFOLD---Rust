// FSR 1.0 RCAS — Robust Contrast-Adaptive Sharpening
// Translated from AMD FidelityFX Super Resolution 1.0 (MIT License, GPUOpen).
//
// Post-process sharpening pass applied to the EASU output.
// Computes a noise-adaptive local sharpening weight from the 5-tap cross
// neighbourhood, applies the sharpening filter, and clamps the result to the
// local min/max to suppress ringing.

struct Uniforms {
    // exp2(−user_sharpness) where user_sharpness ∈ [0.1, 2.0].
    // Lower user_sharpness → higher sharpness effect.
    // Default: exp2(−0.87) ≈ 0.547.
    sharpness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_src: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var t_out: texture_storage_2d<rgba16float, write>;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126729, 0.7151522, 0.0721750));
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(t_out);
    if id.x >= dims.x || id.y >= dims.y { return; }

    let tx = 1.0 / f32(dims.x);
    let ty = 1.0 / f32(dims.y);
    let uv = (vec2<f32>(id.xy) + 0.5) * vec2<f32>(tx, ty);

    // 5-tap cross: centre + 4 cardinal neighbours.
    let n_tap = textureSampleLevel(t_src, s, uv + vec2<f32>( 0.0, -ty), 0.0).rgb;
    let s_tap = textureSampleLevel(t_src, s, uv + vec2<f32>( 0.0,  ty), 0.0).rgb;
    let w_tap = textureSampleLevel(t_src, s, uv + vec2<f32>(-tx,  0.0), 0.0).rgb;
    let e_tap = textureSampleLevel(t_src, s, uv + vec2<f32>( tx,  0.0), 0.0).rgb;
    let cen   = textureSampleLevel(t_src, s, uv,                        0.0).rgb;

    let ln = luma(n_tap); let ls = luma(s_tap);
    let lw = luma(w_tap); let le = luma(e_tap);
    let lc = luma(cen);

    // Local luma range used for noise suppression.
    let min_luma = min(min(ln, ls), min(lw, min(le, lc)));
    let max_luma = max(max(ln, ls), max(lw, max(le, lc)));
    let range    = max_luma - min_luma;

    // Noise-adaptive weight: suppress sharpening in flat/noisy regions.
    // abs(avg_neighbours − centre) / range → ∈ [0, 1] where 1 = high noise.
    let avg_luma  = 0.25 * (ln + ls + lw + le);
    let noise_est = clamp(abs(avg_luma - lc) / max(range, 0.0001), 0.0, 1.0);
    // noise_factor ∈ [0.5, 1.0]: less sharpening where noise is high.
    let noise_factor = 1.0 - 0.5 * noise_est;

    // Sharpening weight: negative (subtracts neighbour average from centre).
    // Bounded to (−0.25, 0) to guarantee a positive normalisation denominator.
    let w = max(-0.25 * u.sharpness * noise_factor, -0.249);
    let norm = 1.0 + 4.0 * w;   // always > 0 given w > -0.25

    // Apply: (neighbours × w + centre) / norm
    var col = ((n_tap + s_tap + w_tap + e_tap) * w + cen) / norm;

    // Anti-ringing: clamp to the local min/max.
    let mn4 = min(min(n_tap, s_tap), min(w_tap, e_tap));
    let mx4 = max(max(n_tap, s_tap), max(w_tap, e_tap));
    col = clamp(col, mn4, mx4);

    textureStore(t_out, vec2<i32>(id.xy), vec4<f32>(col, 1.0));
}
