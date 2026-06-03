// node.saturation — fusable body (freeze §12). Lerp the Rec.709 luma
// grayscale <-> the colour by `saturation`. Pure; alpha passes through.
// Matches saturation.wgsl exactly. PARAMS order: [saturation].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, saturation: f32) -> vec4<f32> {
    let luma = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(mix(vec3<f32>(luma), c.rgb, saturation), c.a);
}
