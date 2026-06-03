// node.film_grain — fusable body (freeze §12), POSITIONAL: own texel modulated
// by a per-pixel white-noise hash. Needs the pixel coordinate pixel = uv*dims
// (recovered from the ambient uv + dims) so the grain stays resolution-locked
// exactly like film_grain.wgsl — both compute uv*dims from the same values, so
// the hash is bit-identical. Pure; alpha passes through. PARAMS order: [amount].
fn white_noise(coord: vec2<f32>) -> f32 {
    return fract(sin(dot(coord, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32) -> vec4<f32> {
    let pixel = uv * dims;
    let noise = white_noise(pixel);
    let rgb = c.rgb * (1.0 - amount * (1.0 - noise));
    return vec4<f32>(rgb, c.a);
}
