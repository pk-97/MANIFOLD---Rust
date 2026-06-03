// node.color_lut — fusable body (freeze §12), MultiInputCoincident with a GATHER
// LUT. `in` is read coincident (the pre-sampled centre colour); `lut` is gathered
// at a 1D coord the body computes from BT.601 luminance (with contrast pivot at
// 0.5), so it arrives as a texture+sampler arg. lum*0.5 bakes the legacy LUT_MAX_
// LUM=2.0 range; result crossfades against the source by `amount`. Matches
// lut1d.wgsl. PARAMS: [amount, contrast]. (uv/dims are ambient and unused here.)
fn body(src: vec4<f32>, lut_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, amount: f32, contrast: f32) -> vec4<f32> {
    let lum_raw = dot(src.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let lum = max(0.0, (lum_raw - 0.5) * contrast + 0.5);
    let lut_coord = clamp(lum * 0.5, 0.0, 1.0);
    let thermal = textureSampleLevel(lut_tex, samp, vec2<f32>(lut_coord, 0.5), 0.0).rgb;
    let result = mix(src.rgb, thermal, amount);
    return vec4<f32>(result, src.a);
}
