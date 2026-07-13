// node.bilateral_blur — fusable body (freeze §12), MultiInputCoincident,
// GATHER (via the STENCIL-FETCH ABI) on `in` + GATHERTEXEL on `depth`
// (docs/CINEMATIC_POST_DESIGN.md D8). Single-axis, depth-guided (bilateral)
// blur: pair an H pass with a V pass for a 2D edge-aware blur between an AO
// atom and its mix (D8's committed denoise; the observed defect: raw AO
// noise with no smoothing pass).
//
// Fixed 9 taps at 1-texel spacing along `axis`. weight_j = K9_j *
// exp(-(dz_j / depth_sigma)^2), where K9_j are the SAME sigma~=2 gaussian
// constants used by every other 9-tap kernel in this codebase (VBW_K9 /
// SG_K9_* — see gaussian_blur_variable_width_body.wgsl / separable_gaussian_
// body.wgsl) and dz_j is the linearized-depth difference between tap j and
// the center texel. Renormalized by the actual weight sum. Alpha is a pure
// center pass-through (D8: "alpha = center pass-through" — the AO atom's
// output is grayscale-with-alpha=1, and the mix downstream reads only RGB
// via Multiply, but a bilateral blur must never blur an alpha channel it
// doesn't own).
//
// `in` is Gather (stencil-fetch: the body samples it at body-computed UV
// offsets via the free `fetch_in(uv)` fn the codegen defines). `depth` is
// GatherTexel: raw texture handle passed in, no sampler — the body does its
// own integer `textureLoad` + manual ClampToEdge at the SAME tap offsets,
// matching `node.ssao_from_depth`'s precedent for depth reads (texel-exact,
// no filtering — the CPU reference replicates integer loads exactly).
// `camera` (near/far only — no fov/aspect needed, this atom never
// reprojects) is consumed ENTIRELY via the two DERIVED_UNIFORMS below, never
// a GPU binding — the D7/P0 mechanism, so this Pointwise-shaped
// MultiInputCoincident atom can still fuse with a neighbour.
//
// PARAMS: [axis (Enum->u32), depth_sigma]. DERIVED_UNIFORMS: [near, far].
// Matches bilateral_blur.wgsl (the hand parity oracle) — kept independent
// (not sharing source) so the gpu_tests parity check is a real cross-check.

// 9-tap (sigma ~= 2.0) — same values as VBW_K9 / SG_K9_* (do not re-derive).
const BB_K9_0: f32 = 0.16501;
const BB_K9_1: f32 = 0.15019;
const BB_K9_2: f32 = 0.11325;
const BB_K9_3: f32 = 0.07076;
const BB_K9_4: f32 = 0.03664;

// Integer-load depth fetch at texel `c`, manual ClampToEdge (no sampler) —
// mirrors `ssao_from_depth_body.wgsl`'s `ssao_view_pos`'s clamp.
fn bb_depth_at(depth_tex: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return textureLoad(depth_tex, cc, 0).r;
}

// One signed tap: returns (weighted rgb, weight) as vec4(rgb, w) — the
// `vbw_tap_group` shape (gaussian_blur_variable_width_body.wgsl), one signed
// offset per call so the caller can sum the +/- pair.
fn bb_tap(
    depth_tex: texture_2d<f32>,
    uv: vec2<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    axis_dir_uv: vec2<f32>,
    axis_dir_texel: vec2<i32>,
    j: i32,
    kj: f32,
    z_center: f32,
    inv_sigma: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let cj = c + axis_dir_texel * j;
    let zj = linearize_depth(bb_depth_at(depth_tex, cj, dims_i), near, far);
    let dz = (zj - z_center) * inv_sigma;
    let w = kj * exp(-(dz * dz));
    let rgb = fetch_in(uv + axis_dir_uv * f32(j)).rgb * w;
    return vec4<f32>(rgb, w);
}

fn body(
    depth_tex: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    axis: u32,
    depth_sigma: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let texel = vec2<f32>(1.0) / dims;
    var axis_dir_uv: vec2<f32>;
    var axis_dir_texel: vec2<i32>;
    if axis == 0u {
        axis_dir_uv = vec2<f32>(texel.x, 0.0);
        axis_dir_texel = vec2<i32>(1, 0);
    } else {
        axis_dir_uv = vec2<f32>(0.0, texel.y);
        axis_dir_texel = vec2<i32>(0, 1);
    }

    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let sigma = max(depth_sigma, 1e-4);
    let inv_sigma = 1.0 / sigma;

    let center = fetch_in(uv);
    let z_center = linearize_depth(bb_depth_at(depth_tex, c, dims_i), near, far);

    var acc = center.rgb * BB_K9_0;
    var wsum = BB_K9_0;

    let t1p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 1, BB_K9_1, z_center, inv_sigma, near, far);
    let t1m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -1, BB_K9_1, z_center, inv_sigma, near, far);
    acc += t1p.rgb + t1m.rgb;
    wsum += t1p.a + t1m.a;

    let t2p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 2, BB_K9_2, z_center, inv_sigma, near, far);
    let t2m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -2, BB_K9_2, z_center, inv_sigma, near, far);
    acc += t2p.rgb + t2m.rgb;
    wsum += t2p.a + t2m.a;

    let t3p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 3, BB_K9_3, z_center, inv_sigma, near, far);
    let t3m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -3, BB_K9_3, z_center, inv_sigma, near, far);
    acc += t3p.rgb + t3m.rgb;
    wsum += t3p.a + t3m.a;

    let t4p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 4, BB_K9_4, z_center, inv_sigma, near, far);
    let t4m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -4, BB_K9_4, z_center, inv_sigma, near, far);
    acc += t4p.rgb + t4m.rgb;
    wsum += t4p.a + t4m.a;

    let rgb = acc / max(wsum, 1e-6);
    return vec4<f32>(rgb, center.a);
}
