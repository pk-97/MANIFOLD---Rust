// node.convolution_2d_9tap — general 3×3 non-separable convolution
// with a uniform-supplied kernel. New WGSL for the buffer-port
// primitive vocabulary; the legacy Watercolor diffusion uses a
// similar 9-tap pattern, but with weights hardcoded inline.
//
// Kernel layout (row-major, 3×3):
//   k0 k1 k2
//   k3 k4 k5
//   k6 k7 k8
// k4 is the center weight.
//
// Output is normalised by sum(weights) by default; set `normalise =
// 0` to skip normalisation (useful for kernels that don't sum to 1
// like edge filters).

struct ConvUniforms {
    k0: f32, k1: f32, k2: f32, k3: f32,
    k4: f32, k5: f32, k6: f32, k7: f32,
    k8: f32,
    bias: f32,
    normalise: u32,
    _pad0: u32,
};

@group(0) @binding(0) var<uniform> u: ConvUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let texel = 1.0 / vec2<f32>(dims);
    let uv = (vec2<f32>(gid.xy) + 0.5) * texel;

    let p00 = textureSampleLevel(t_source, s_source, uv + vec2<f32>(-texel.x, -texel.y), 0.0).rgb;
    let p10 = textureSampleLevel(t_source, s_source, uv + vec2<f32>( 0.0,     -texel.y), 0.0).rgb;
    let p20 = textureSampleLevel(t_source, s_source, uv + vec2<f32>( texel.x, -texel.y), 0.0).rgb;
    let p01 = textureSampleLevel(t_source, s_source, uv + vec2<f32>(-texel.x,  0.0    ), 0.0).rgb;
    let center_sample = textureSampleLevel(t_source, s_source, uv, 0.0);
    let p11 = center_sample.rgb;
    let p21 = textureSampleLevel(t_source, s_source, uv + vec2<f32>( texel.x,  0.0    ), 0.0).rgb;
    let p02 = textureSampleLevel(t_source, s_source, uv + vec2<f32>(-texel.x,  texel.y), 0.0).rgb;
    let p12 = textureSampleLevel(t_source, s_source, uv + vec2<f32>( 0.0,      texel.y), 0.0).rgb;
    let p22 = textureSampleLevel(t_source, s_source, uv + vec2<f32>( texel.x,  texel.y), 0.0).rgb;

    var sum = p00 * u.k0 + p10 * u.k1 + p20 * u.k2
            + p01 * u.k3 + p11 * u.k4 + p21 * u.k5
            + p02 * u.k6 + p12 * u.k7 + p22 * u.k8;

    if u.normalise == 1u {
        let weight_sum = u.k0 + u.k1 + u.k2 + u.k3 + u.k4 + u.k5 + u.k6 + u.k7 + u.k8;
        if abs(weight_sum) > 1e-6 {
            sum = sum / weight_sum;
        }
    }

    sum = sum + vec3<f32>(u.bias);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(sum, center_sample.a));
}
