// Black Hole — Separable Gaussian Blur
//
// 21-tap σ=4 Gaussian, separable into horizontal then vertical pass.
// Runs over the deflection bake's quarter-res textures so the cost
// is small (~14 samples per full-res pixel total across both passes
// and all three textures, vs 507 for an inline 13×13 σ=3 kernel).
//
// Two entry points share the same uniform/texture layout — Naga's
// multi-entry-point uniform-size rule is satisfied because they
// have identical bindings.

struct BlurUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: BlurUniforms;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

// 21-tap (radius 10) Gaussian, sigma = 4.0. Weights are pre-normalized
// (sum = 1.0) and symmetric so we only store one half.
//
//   w(i) = exp(-i² / (2σ²)) / Σ
//
// With σ=4, divisor = 32.0.
fn gauss(i: i32) -> f32 {
    let f = f32(i);
    return exp(-(f * f) / 32.0);
}

@compute @workgroup_size(16, 16, 1)
fn blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.width || gid.y >= u.height {
        return;
    }
    let inv = vec2<f32>(1.0 / f32(u.width), 1.0 / f32(u.height));
    let uv = (vec2<f32>(gid.xy) + 0.5) * inv;

    var sum = vec4<f32>(0.0);
    var w_total = 0.0;
    for (var i: i32 = -10; i <= 10; i = i + 1) {
        let off = vec2<f32>(f32(i) * inv.x, 0.0);
        let w = gauss(i);
        sum = sum + textureSampleLevel(src, samp, uv + off, 0.0) * w;
        w_total = w_total + w;
    }
    sum = sum / w_total;
    textureStore(dst, vec2<i32>(gid.xy), sum);
}

@compute @workgroup_size(16, 16, 1)
fn blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.width || gid.y >= u.height {
        return;
    }
    let inv = vec2<f32>(1.0 / f32(u.width), 1.0 / f32(u.height));
    let uv = (vec2<f32>(gid.xy) + 0.5) * inv;

    var sum = vec4<f32>(0.0);
    var w_total = 0.0;
    for (var i: i32 = -10; i <= 10; i = i + 1) {
        let off = vec2<f32>(0.0, f32(i) * inv.y);
        let w = gauss(i);
        sum = sum + textureSampleLevel(src, samp, uv + off, 0.0) * w;
        w_total = w_total + w;
    }
    sum = sum / w_total;
    textureStore(dst, vec2<i32>(gid.xy), sum);
}
