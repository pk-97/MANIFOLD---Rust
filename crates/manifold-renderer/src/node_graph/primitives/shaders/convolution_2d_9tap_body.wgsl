// node.convolution_2d_9tap — fusable body (freeze §12), GATHER. General 3×3
// non-separable convolution with a uniform-supplied kernel. `in` is gathered at
// the 9 neighbours (row-major k0..k8, k4 = centre; texel = 1/dims recovered from
// the ambient dims). normalise=1 divides by sum(weights); bias is added; alpha
// follows the centre tap. Matches convolution_2d_9tap.wgsl. PARAMS: [k0..k8,
// bias, normalise (Bool->u32)].
fn body(
    t_source: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>,
    k0: f32, k1: f32, k2: f32, k3: f32, k4: f32, k5: f32, k6: f32, k7: f32, k8: f32,
    bias: f32, normalise: u32,
) -> vec4<f32> {
    let texel = 1.0 / dims;

    let p00 = textureSampleLevel(t_source, samp, uv + vec2<f32>(-texel.x, -texel.y), 0.0).rgb;
    let p10 = textureSampleLevel(t_source, samp, uv + vec2<f32>( 0.0,     -texel.y), 0.0).rgb;
    let p20 = textureSampleLevel(t_source, samp, uv + vec2<f32>( texel.x, -texel.y), 0.0).rgb;
    let p01 = textureSampleLevel(t_source, samp, uv + vec2<f32>(-texel.x,  0.0    ), 0.0).rgb;
    let center_sample = textureSampleLevel(t_source, samp, uv, 0.0);
    let p11 = center_sample.rgb;
    let p21 = textureSampleLevel(t_source, samp, uv + vec2<f32>( texel.x,  0.0    ), 0.0).rgb;
    let p02 = textureSampleLevel(t_source, samp, uv + vec2<f32>(-texel.x,  texel.y), 0.0).rgb;
    let p12 = textureSampleLevel(t_source, samp, uv + vec2<f32>( 0.0,      texel.y), 0.0).rgb;
    let p22 = textureSampleLevel(t_source, samp, uv + vec2<f32>( texel.x,  texel.y), 0.0).rgb;

    var sum = p00 * k0 + p10 * k1 + p20 * k2
            + p01 * k3 + p11 * k4 + p21 * k5
            + p02 * k6 + p12 * k7 + p22 * k8;

    if normalise == 1u {
        let weight_sum = k0 + k1 + k2 + k3 + k4 + k5 + k6 + k7 + k8;
        if abs(weight_sum) > 1e-6 {
            sum = sum / weight_sum;
        }
    }

    sum = sum + vec3<f32>(bias);

    return vec4<f32>(sum, center_sample.a);
}
