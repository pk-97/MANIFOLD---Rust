// node.power_texture — fusable body (freeze §12). Per-pixel
// pow(max(rgb, 0), exponent), alpha pass-through. max(_,0) guards pow(negative).
// Pure own-texel. Matches power_texture.wgsl. PARAMS: [exponent].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, exponent: f32) -> vec4<f32> {
    return vec4<f32>(
        pow(max(c.r, 0.0), exponent),
        pow(max(c.g, 0.0), exponent),
        pow(max(c.b, 0.0), exponent),
        c.a,
    );
}
