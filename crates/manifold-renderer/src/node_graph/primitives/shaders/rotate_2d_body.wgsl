// node.rotate_coordinates — fusable body (freeze §12). Rotates a 2-channel
// coordinate texture (R=x, G=y) by `angle` radians around the origin.
// Output B=0, A=1 — this is a coordinate transform, not a colour
// passthrough, so alpha is NOT taken from the input (matches
// rotate_2d.wgsl exactly). PARAMS order: [angle].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, angle: f32) -> vec4<f32> {
    let cos_a = cos(angle);
    let sin_a = sin(angle);
    let rx = c.r * cos_a - c.g * sin_a;
    let ry = c.r * sin_a + c.g * cos_a;
    return vec4<f32>(rx, ry, 0.0, 1.0);
}
