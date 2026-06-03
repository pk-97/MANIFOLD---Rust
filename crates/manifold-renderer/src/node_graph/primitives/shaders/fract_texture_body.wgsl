// node.fract_texture — fusable body (freeze §12). Per-pixel fract(rgb * scale),
// alpha pass-through. Pure own-texel. Matches fract_texture.wgsl. PARAMS: [scale].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32) -> vec4<f32> {
    return vec4<f32>(fract(c.r * scale), fract(c.g * scale), fract(c.b * scale), c.a);
}
