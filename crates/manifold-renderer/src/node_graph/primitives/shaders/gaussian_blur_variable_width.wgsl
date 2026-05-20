// node.gaussian_blur_variable_width — separable Gaussian blur
// where the per-pixel kernel width is sampled from a Texture2D
// width map. One dispatch = one axis (X or Y); pair two
// dispatches with ping-pong for a full 2D blur.
//
// Adapted from effects/shaders/fx_depth_of_field_compute.wgsl
// blur_17tap, with the width source decoupled from input.alpha
// (it's a separate Texture2D input now) so the primitive composes
// with any width source.
//
// Quality fixed at 17-tap to keep the primitive single-pipeline.
// Wider blurs come from the per-pixel width input; tighter from
// near-zero width. A future "quality" enum could swap to 9 or 25
// taps if needed.

struct BlurUniforms {
    direction: u32,       // 0 = horizontal, 1 = vertical
    max_radius: f32,      // hard cap on width sample × max_radius (default 12.0 like DoF)
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: BlurUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var t_width: texture_2d<f32>;
@group(0) @binding(3) var s_source: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// 17-tap Gaussian kernel weights (sigma ≈ 4.0). Same as DoF's K17_*.
const K0: f32 = 0.10315;
const K1: f32 = 0.09998;
const K2: f32 = 0.09103;
const K3: f32 = 0.07786;
const K4: f32 = 0.06257;
const K5: f32 = 0.04723;
const K6: f32 = 0.03350;
const K7: f32 = 0.02232;
const K8: f32 = 0.01396;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let center = textureSampleLevel(t_source, s_source, uv, 0.0);

    // Sample per-pixel width (R channel). Pre-clamped to [0, 1] then
    // scaled by max_radius. Zero width = no blur (pass-through).
    let width_raw = clamp(
        textureSampleLevel(t_width, s_source, uv, 0.0).r,
        0.0,
        1.0,
    );
    if width_raw < 0.005 {
        textureStore(output_tex, vec2<i32>(gid.xy), center);
        return;
    }

    let step_size = width_raw * u.max_radius + 1.0;
    let texel = 1.0 / vec2<f32>(dims);
    let dir = select(
        vec2<f32>(0.0, texel.y),
        vec2<f32>(texel.x, 0.0),
        u.direction == 0u,
    );
    let d = dir * step_size;

    var acc = center.rgb * K0;
    var w_acc: f32 = K0;

    acc += textureSampleLevel(t_source, s_source, uv + d,         0.0).rgb * K1; w_acc += K1;
    acc += textureSampleLevel(t_source, s_source, uv - d,         0.0).rgb * K1; w_acc += K1;
    acc += textureSampleLevel(t_source, s_source, uv + d * 2.0,   0.0).rgb * K2; w_acc += K2;
    acc += textureSampleLevel(t_source, s_source, uv - d * 2.0,   0.0).rgb * K2; w_acc += K2;
    acc += textureSampleLevel(t_source, s_source, uv + d * 3.0,   0.0).rgb * K3; w_acc += K3;
    acc += textureSampleLevel(t_source, s_source, uv - d * 3.0,   0.0).rgb * K3; w_acc += K3;
    acc += textureSampleLevel(t_source, s_source, uv + d * 4.0,   0.0).rgb * K4; w_acc += K4;
    acc += textureSampleLevel(t_source, s_source, uv - d * 4.0,   0.0).rgb * K4; w_acc += K4;
    acc += textureSampleLevel(t_source, s_source, uv + d * 5.0,   0.0).rgb * K5; w_acc += K5;
    acc += textureSampleLevel(t_source, s_source, uv - d * 5.0,   0.0).rgb * K5; w_acc += K5;
    acc += textureSampleLevel(t_source, s_source, uv + d * 6.0,   0.0).rgb * K6; w_acc += K6;
    acc += textureSampleLevel(t_source, s_source, uv - d * 6.0,   0.0).rgb * K6; w_acc += K6;
    acc += textureSampleLevel(t_source, s_source, uv + d * 7.0,   0.0).rgb * K7; w_acc += K7;
    acc += textureSampleLevel(t_source, s_source, uv - d * 7.0,   0.0).rgb * K7; w_acc += K7;
    acc += textureSampleLevel(t_source, s_source, uv + d * 8.0,   0.0).rgb * K8; w_acc += K8;
    acc += textureSampleLevel(t_source, s_source, uv - d * 8.0,   0.0).rgb * K8; w_acc += K8;

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(acc / max(w_acc, 0.001), center.a));
}
