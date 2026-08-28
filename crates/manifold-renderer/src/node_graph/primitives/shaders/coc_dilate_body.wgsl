// node.coc_dilate — fusable body (freeze section 12), Pointwise + sampler-Gather
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
// Input convention (matches coc_from_depth_body.wgsl's output):
// R and B hold coc_px / max_radius (the [0,1] magnitude), G holds the
// sign flag (1.0 = nearer than focus, 0.0 = far-or-in-focus), alpha == 1.0.
// Output: the 3x3 neighborhood max is computed independently for R and G;
// B copies the R max, alpha == 1.0. G max == 1.0 means "any pixel in the
// neighborhood is nearer than focus" (docs/BOKEH_LAYERED_DOF_DESIGN.md D1).
//
// PARAMS: none. Matches coc_dilate.wgsl (the hand parity oracle).

fn body(uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    let texel = vec2<f32>(1.0) / dims;

    // Fixed 3x3 neighborhood max, unrolled (matches the codebase's
    // unrolled-tap convention, e.g. separable_gaussian_body.wgsl's
    // sg_blur_9/17/25 — no loops/branches to keep spirv-opt's DCE/inline
    // passes fully effective, single-exit per the sg_blur_linear note).
    //
    // R and G are maxed independently; B copies R so the magnitude
    // convention remains self-describing for any downstream reader that
    // still expects RGB to match (none do, but the channel is cheap).
    var m_r: f32 = fetch_in(uv).r;
    m_r = max(m_r, fetch_in(uv + vec2<f32>(-texel.x, -texel.y)).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>(0.0,      -texel.y)).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>( texel.x, -texel.y)).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>(-texel.x, 0.0     )).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>( texel.x, 0.0     )).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>(-texel.x,  texel.y)).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>(0.0,       texel.y)).r);
    m_r = max(m_r, fetch_in(uv + vec2<f32>( texel.x,  texel.y)).r);

    var m_g: f32 = fetch_in(uv).g;
    m_g = max(m_g, fetch_in(uv + vec2<f32>(-texel.x, -texel.y)).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>(0.0,      -texel.y)).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>( texel.x, -texel.y)).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>(-texel.x, 0.0     )).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>( texel.x, 0.0     )).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>(-texel.x,  texel.y)).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>(0.0,       texel.y)).g);
    m_g = max(m_g, fetch_in(uv + vec2<f32>( texel.x,  texel.y)).g);

    return vec4<f32>(m_r, m_g, m_r, 1.0);
}
