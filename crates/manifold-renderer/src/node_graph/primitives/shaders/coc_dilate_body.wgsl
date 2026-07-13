// node.coc_dilate — fusable body (freeze §12), Pointwise + sampler-Gather
// (STENCIL-FETCH ABI). Fixed 3x3 neighborhood max of the input texture's R
// channel — BUG-137's committed fix shape (docs/BUG_BACKLOG.md): spread the
// maximum CoC found in a small neighborhood outward so `node.variable_blur`'s
// per-pixel gather radius can borrow a wider radius from an adjacent
// high-CoC pixel, softening the hard seam at depth discontinuities.
//
// `in` is a Gather input: the body reads it through `fetch_in(uv)` — defined
// by the codegen as the real textureSampleLevel over the bound texture
// (standalone / fused real external), or as a recomputed upstream chain
// (fused virtual source). Matches separable_gaussian.wgsl's stencil-fetch
// ABI. No params, no derived uniforms — the 3x3 radius is fixed (quality
// plumbing, not a performer knob, per D8's `bilateral_blur` precedent).
//
// Input convention (matches coc_from_depth_body.wgsl's output exactly):
// R == G == B == coc_px / max_radius (a [0,1] fraction), alpha == 1.0.
// Output: same convention — the max found in the 3x3 neighborhood,
// broadcast to RGB, alpha == 1.0 (center pass-through per the design
// prompt's alpha note).
//
// PARAMS: none. Matches coc_dilate.wgsl (the hand parity oracle).

fn body(uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    let texel = vec2<f32>(1.0) / dims;

    // Fixed 3x3 neighborhood max, unrolled (matches the codebase's
    // unrolled-tap convention, e.g. separable_gaussian_body.wgsl's
    // sg_blur_9/17/25 — no loops/branches to keep spirv-opt's DCE/inline
    // passes fully effective, single-exit per the sg_blur_linear note).
    var m: f32 = fetch_in(uv).r;
    m = max(m, fetch_in(uv + vec2<f32>(-texel.x, -texel.y)).r);
    m = max(m, fetch_in(uv + vec2<f32>(0.0,      -texel.y)).r);
    m = max(m, fetch_in(uv + vec2<f32>( texel.x, -texel.y)).r);
    m = max(m, fetch_in(uv + vec2<f32>(-texel.x, 0.0     )).r);
    m = max(m, fetch_in(uv + vec2<f32>( texel.x, 0.0     )).r);
    m = max(m, fetch_in(uv + vec2<f32>(-texel.x,  texel.y)).r);
    m = max(m, fetch_in(uv + vec2<f32>(0.0,       texel.y)).r);
    m = max(m, fetch_in(uv + vec2<f32>( texel.x,  texel.y)).r);

    return vec4<f32>(m, m, m, 1.0);
}
