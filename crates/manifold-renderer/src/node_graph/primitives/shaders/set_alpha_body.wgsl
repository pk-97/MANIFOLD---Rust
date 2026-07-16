// node.set_alpha — fusable body (freeze §12). RGB pass-through, alpha
// forced to the `alpha` param. Pure own-texel. Matches set_alpha.wgsl.
// PARAMS: [alpha].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, alpha: f32) -> vec4<f32> {
    return vec4<f32>(c.rgb, alpha);
}
