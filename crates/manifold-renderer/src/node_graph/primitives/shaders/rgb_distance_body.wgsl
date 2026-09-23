// node.rgb_distance — fusable body (freeze section 12), Pointwise.
// RGB Euclidean distance from a scalar-bindable target colour. Alpha is always
// one so the result is a complete grayscale distance texture.
// PARAMS: [red, green, blue].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, red: f32, green: f32, blue: f32) -> vec4<f32> {
    let target_rgb = clamp(vec3<f32>(red, green, blue), vec3<f32>(0.0), vec3<f32>(1.0));
    let distance = length(c.rgb - target_rgb);
    return vec4<f32>(distance, distance, distance, 1.0);
}
