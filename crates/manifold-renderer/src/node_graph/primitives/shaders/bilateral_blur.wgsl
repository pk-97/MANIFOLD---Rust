// node.bilateral_blur — hand parity oracle for the generated standalone
// kernel (docs/CINEMATIC_POST_DESIGN.md D8). Same fixed-9-tap depth-guided
// blur as bilateral_blur_body.wgsl — kept independent (not sharing source)
// so the gpu_tests parity check is a real cross-check, not a tautology.
//
// Bindings match the generated MultiInputCoincident([Gather, GatherTexel])
// layout: uniform(0), in_tex(1, sampled), depth_tex(2, textureLoad — no
// sampler use on depth), samp(3, bound for `in`), output_tex(4).

struct Uniforms {
    axis: u32,
    depth_sigma: f32,
    near: f32,
    far: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var in_tex: texture_2d<f32>;
@group(0) @binding(2) var depth_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

const K9_0: f32 = 0.16501;
const K9_1: f32 = 0.15019;
const K9_2: f32 = 0.11325;
const K9_3: f32 = 0.07076;
const K9_4: f32 = 0.03664;

fn depth_at(c: vec2<i32>, dims_i: vec2<i32>) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return textureLoad(depth_tex, cc, 0).r;
}

fn tap(
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
    let zj = linearize_depth(depth_at(cj, dims_i), near, far);
    let dz = (zj - z_center) * inv_sigma;
    let w = kj * exp(-(dz * dz));
    let rgb = textureSampleLevel(in_tex, samp, uv + axis_dir_uv * f32(j), 0.0).rgb * w;
    return vec4<f32>(rgb, w);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }

    let dims_f = vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5, 0.5)) / dims_f;
    let texel = vec2<f32>(1.0) / dims_f;

    var axis_dir_uv: vec2<f32>;
    var axis_dir_texel: vec2<i32>;
    if u.axis == 0u {
        axis_dir_uv = vec2<f32>(texel.x, 0.0);
        axis_dir_texel = vec2<i32>(1, 0);
    } else {
        axis_dir_uv = vec2<f32>(0.0, texel.y);
        axis_dir_texel = vec2<i32>(0, 1);
    }

    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(id.xy);
    let sigma = max(u.depth_sigma, 1e-4);
    let inv_sigma = 1.0 / sigma;

    let center = textureSampleLevel(in_tex, samp, uv, 0.0);
    let z_center = linearize_depth(depth_at(c, dims_i), u.near, u.far);

    var acc = center.rgb * K9_0;
    var wsum = K9_0;

    let t1p = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, 1, K9_1, z_center, inv_sigma, u.near, u.far);
    let t1m = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, -1, K9_1, z_center, inv_sigma, u.near, u.far);
    acc += t1p.rgb + t1m.rgb;
    wsum += t1p.a + t1m.a;

    let t2p = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, 2, K9_2, z_center, inv_sigma, u.near, u.far);
    let t2m = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, -2, K9_2, z_center, inv_sigma, u.near, u.far);
    acc += t2p.rgb + t2m.rgb;
    wsum += t2p.a + t2m.a;

    let t3p = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, 3, K9_3, z_center, inv_sigma, u.near, u.far);
    let t3m = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, -3, K9_3, z_center, inv_sigma, u.near, u.far);
    acc += t3p.rgb + t3m.rgb;
    wsum += t3p.a + t3m.a;

    let t4p = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, 4, K9_4, z_center, inv_sigma, u.near, u.far);
    let t4m = tap(uv, c, dims_i, axis_dir_uv, axis_dir_texel, -4, K9_4, z_center, inv_sigma, u.near, u.far);
    acc += t4p.rgb + t4m.rgb;
    wsum += t4p.a + t4m.a;

    let rgb = acc / max(wsum, 1e-6);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, center.a));
}
