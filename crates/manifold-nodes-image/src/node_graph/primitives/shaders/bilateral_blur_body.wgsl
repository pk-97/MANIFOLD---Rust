// node.bilateral_blur — fusable body, MultiInputCoincident: Gather (stencil
// fetch) on `in`, GatherTexel on `depth`.
// Single-axis depth-guided blur; pair an H pass with a V pass.
//
// Fixed 9 taps at 1-texel spacing along `axis`. weight_j = K9_j *
// exp(-(dz_j / depth_sigma)^2), with K9_j the sigma~=2 gaussian constants
// every other 9-tap kernel here uses (VBW_K9 / SG_K9_*), renormalized by
// the weight sum. dz_j is tap j's view depth minus the depth the centre's
// surface has at tap j: the centre's raw depth stepped j texels along its
// gentler one-texel raw slope. Raw reversed-Z depth is affine in screen
// space on a plane, so a flat surface predicts each tap exactly, however
// steeply it recedes; an absolute depth difference would reject the
// neighbours of grazing ground across rows. The gentler side is taken so a
// silhouette next to the centre never sets the slope. Alpha is the centre's,
// passed through.
//
// `depth` is read texel-exact (integer textureLoad, manual ClampToEdge).
// `camera` reaches the body only through the near/far derived uniforms.

const BB_K9_0: f32 = 0.16501;
const BB_K9_1: f32 = 0.15019;
const BB_K9_2: f32 = 0.11325;
const BB_K9_3: f32 = 0.07076;
const BB_K9_4: f32 = 0.03664;

fn bb_depth_at(depth_tex: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return textureLoad(depth_tex, cc, 0).r;
}

// One signed tap: (weighted rgb, weight).
fn bb_tap(
    depth_tex: texture_2d<f32>,
    uv: vec2<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    axis_dir_uv: vec2<f32>,
    axis_dir_texel: vec2<i32>,
    j: i32,
    kj: f32,
    raw_center: f32,
    raw_slope: f32,
    inv_sigma: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let cj = c + axis_dir_texel * j;
    let zj = linearize_depth(bb_depth_at(depth_tex, cj, dims_i), near, far);
    let z_surface = linearize_depth(max(raw_center + raw_slope * f32(j), 0.0), near, far);
    let dz = (zj - z_surface) * inv_sigma;
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
    let raw_center = bb_depth_at(depth_tex, c, dims_i);
    let ahead = bb_depth_at(depth_tex, c + axis_dir_texel, dims_i) - raw_center;
    let behind = raw_center - bb_depth_at(depth_tex, c - axis_dir_texel, dims_i);
    let raw_slope = select(behind, ahead, abs(ahead) < abs(behind));

    var acc = center.rgb * BB_K9_0;
    var wsum = BB_K9_0;

    let t1p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 1, BB_K9_1, raw_center, raw_slope, inv_sigma, near, far);
    let t1m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -1, BB_K9_1, raw_center, raw_slope, inv_sigma, near, far);
    acc += t1p.rgb + t1m.rgb;
    wsum += t1p.a + t1m.a;

    let t2p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 2, BB_K9_2, raw_center, raw_slope, inv_sigma, near, far);
    let t2m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -2, BB_K9_2, raw_center, raw_slope, inv_sigma, near, far);
    acc += t2p.rgb + t2m.rgb;
    wsum += t2p.a + t2m.a;

    let t3p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 3, BB_K9_3, raw_center, raw_slope, inv_sigma, near, far);
    let t3m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -3, BB_K9_3, raw_center, raw_slope, inv_sigma, near, far);
    acc += t3p.rgb + t3m.rgb;
    wsum += t3p.a + t3m.a;

    let t4p = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, 4, BB_K9_4, raw_center, raw_slope, inv_sigma, near, far);
    let t4m = bb_tap(depth_tex, uv, c, dims_i, axis_dir_uv, axis_dir_texel, -4, BB_K9_4, raw_center, raw_slope, inv_sigma, near, far);
    acc += t4p.rgb + t4m.rgb;
    wsum += t4p.a + t4m.a;

    let rgb = acc / max(wsum, 1e-6);
    return vec4<f32>(rgb, center.a);
}
