// node.bilateral_blur — fusable body (freeze section 12), MultiInputCoincident,
// GATHERTEXEL on `in` + `depth` (+ optional `coverage`) (S6 revision: all
// integer textureLoad — `in` may carry fp32 clip depth in ClipDepth mode,
// and r32float is NOT filterable, so the sampler-based stencil-fetch ABI is
// out for the depth path; every tap is an integer 1-texel offset anyway, so
// texel-exact loads are the right semantics for both modes).
//
// Single-axis, depth-guided (bilateral) blur: pair an H pass with a V pass
// for a 2D edge-aware blur (docs/CINEMATIC_POST_DESIGN.md D8).
//
// Fixed 9 taps at configurable integer texel spacing along `axis`. weight_j = K9_j *
// exp(-(dz_j / depth_sigma)^2), where K9_j are the SAME sigma~=2 gaussian
// constants used by every other 9-tap kernel in this codebase (VBW_K9 /
// SG_K9_* — see gaussian_blur_variable_width_body.wgsl / separable_gaussian_
// body.wgsl) and dz_j is the linearized-depth difference between tap j and
// the center texel. Renormalized by the actual weight sum. Alpha is a pure
// center pass-through (D8) — a bilateral blur must never blur an alpha
// channel it doesn't own.
//
// value_space (S6): RawColour (default) averages `in`'s values as-is —
// byte-identical to the pre-S6 kernel's values (texel-centre sampler reads
// and integer loads agree at the integer tap offsets both use). ClipDepth
// treats `in` as raw [0,1] clip depth: each tap is linearized through the
// shared projection convention (depth_common.wgsl), the WEIGHTED AVERAGE
// runs in linear eye depth, and the result converts back through
// delinearize_depth. The guide is the raw `depth` input in both modes.
// Requires the `camera` (near/far) — never an f16 intermediate (design:
// fp32 depth only, and the fp32 depth texture must never be sampler-read).
//
// coverage (S6, optional): wired, taps whose coverage texel is uncovered
// are EXCLUDED from both the weighted sum and the weight total, and an
// uncovered CENTRE pixel passes through untouched (empty pixels are never
// smoothed into liquid). Unwired is byte-identical to the pre-S6 kernel.
//
// `camera` (near/far only — no fov/aspect needed for depth linearization)
// is consumed ENTIRELY via DERIVED_UNIFORMS, never a GPU binding — the
// D7/P0 mechanism, so this Pointwise-shaped MultiInputCoincident atom can
// still fuse with a neighbour.
//
// PARAMS: [axis (Enum->u32), depth_sigma, value_space (Enum->u32)].
// DERIVED_UNIFORMS: [near, far]. Injected: [use_coverage].

const BB_K9_0: f32 = 0.16501;
const BB_K9_1: f32 = 0.15019;
const BB_K9_2: f32 = 0.11325;
const BB_K9_3: f32 = 0.07076;
const BB_K9_4: f32 = 0.03664;

// Integer-load fetch at texel `c`, manual ClampToEdge (no sampler).
fn bb_load_at(tex: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>) -> vec4<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return textureLoad(tex, cc, 0);
}

// One signed tap: returns (weighted rgb, weight) as vec4(rgb, w) — the
// `vbw_tap_group` shape (gaussian_blur_variable_width_body.wgsl), one signed
// offset per call so the caller can sum the +/- pair. `skip` drops the tap
// entirely (coverage exclusion): no weight, no value.
fn bb_tap(
    in_tex: texture_2d<f32>,
    depth_tex: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    axis_dir_texel: vec2<i32>,
    j: i32,
    kj: f32,
    z_center: f32,
    inv_sigma: f32,
    near: f32,
    far: f32,
    value_space: u32,
    skip: bool,
) -> vec4<f32> {
    if (skip) {
        return vec4<f32>(0.0);
    }
    let cj = c + axis_dir_texel * j;
    let zj = linearize_depth(bb_load_at(depth_tex, cj, dims_i).r, near, far);
    let dz = (zj - z_center) * inv_sigma;
    let w = kj * exp(-(dz * dz));
    var rgb = bb_load_at(in_tex, cj, dims_i).rgb;
    // ClipDepth (S6): average in linear eye depth, convert back below.
    if (value_space == 1u) {
        rgb = vec3<f32>(linearize_depth(rgb.r, near, far));
    }
    return vec4<f32>(rgb * w, w);
}

fn body(
    in_tex: texture_2d<f32>,
    depth_tex: texture_2d<f32>,
    coverage_tex: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    axis: u32,
    depth_sigma: f32,
    spatial_step: f32,
    value_space: u32,
    near: f32,
    far: f32,
    use_coverage: u32,
) -> vec4<f32> {
    var axis_dir_texel: vec2<i32>;
    if axis == 0u {
        axis_dir_texel = vec2<i32>(1, 0);
    } else {
        axis_dir_texel = vec2<i32>(0, 1);
    }

    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let sigma = max(depth_sigma, 1e-4);
    let inv_sigma = 1.0 / sigma;

    let center = bb_load_at(in_tex, c, dims_i);
    let center_cov = use_coverage != 0u && bb_load_at(coverage_tex, c, dims_i).r < 0.5;
    if (center_cov) {
        // Uncovered centre: pass through untouched (empty pixels are never
        // smoothed into liquid).
        return center;
    }
    let z_center = linearize_depth(bb_load_at(depth_tex, c, dims_i).r, near, far);

    var acc = center.rgb * BB_K9_0;
    var wsum = BB_K9_0;
    if (value_space == 1u) {
        acc = vec3<f32>(linearize_depth(center.r, near, far)) * BB_K9_0;
    }

    let step = max(round(spatial_step), 1.0);
    let s1p = use_coverage != 0u && bb_load_at(coverage_tex, c + axis_dir_texel * i32(step), dims_i).r < 0.5;
    let s1m = use_coverage != 0u && bb_load_at(coverage_tex, c - axis_dir_texel * i32(step), dims_i).r < 0.5;
    let t1p = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, i32(step), BB_K9_1, z_center, inv_sigma, near, far, value_space, s1p);
    let t1m = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, -i32(step), BB_K9_1, z_center, inv_sigma, near, far, value_space, s1m);
    acc += t1p.rgb + t1m.rgb;
    wsum += t1p.a + t1m.a;

    let s2p = use_coverage != 0u && bb_load_at(coverage_tex, c + axis_dir_texel * i32(2.0 * step), dims_i).r < 0.5;
    let s2m = use_coverage != 0u && bb_load_at(coverage_tex, c - axis_dir_texel * i32(2.0 * step), dims_i).r < 0.5;
    let t2p = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, i32(2.0 * step), BB_K9_2, z_center, inv_sigma, near, far, value_space, s2p);
    let t2m = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, -i32(2.0 * step), BB_K9_2, z_center, inv_sigma, near, far, value_space, s2m);
    acc += t2p.rgb + t2m.rgb;
    wsum += t2p.a + t2m.a;

    let s3p = use_coverage != 0u && bb_load_at(coverage_tex, c + axis_dir_texel * i32(3.0 * step), dims_i).r < 0.5;
    let s3m = use_coverage != 0u && bb_load_at(coverage_tex, c - axis_dir_texel * i32(3.0 * step), dims_i).r < 0.5;
    let t3p = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, i32(3.0 * step), BB_K9_3, z_center, inv_sigma, near, far, value_space, s3p);
    let t3m = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, -i32(3.0 * step), BB_K9_3, z_center, inv_sigma, near, far, value_space, s3m);
    acc += t3p.rgb + t3m.rgb;
    wsum += t3p.a + t3m.a;

    let s4p = use_coverage != 0u && bb_load_at(coverage_tex, c + axis_dir_texel * i32(4.0 * step), dims_i).r < 0.5;
    let s4m = use_coverage != 0u && bb_load_at(coverage_tex, c - axis_dir_texel * i32(4.0 * step), dims_i).r < 0.5;
    let t4p = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, i32(4.0 * step), BB_K9_4, z_center, inv_sigma, near, far, value_space, s4p);
    let t4m = bb_tap(in_tex, depth_tex, c, dims_i, axis_dir_texel, -i32(4.0 * step), BB_K9_4, z_center, inv_sigma, near, far, value_space, s4m);
    acc += t4p.rgb + t4m.rgb;
    wsum += t4p.a + t4m.a;

    var rgb = acc / max(wsum, 1e-6);
    if (value_space == 1u) {
        rgb = vec3<f32>(clamp(delinearize_depth(rgb.r, near, far), 0.0, 0.99999994));
    }
    return vec4<f32>(rgb, center.a);
}
