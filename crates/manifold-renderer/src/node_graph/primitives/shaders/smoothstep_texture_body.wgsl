// node.smoothstep_texture — fusable body (freeze §12). Per-pixel
// smoothstep(low, high, rgb), alpha pass-through. low>high inverts (smoothstep
// flips). Pure own-texel. Matches smoothstep_texture.wgsl. PARAMS: [low, high].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, low: f32, high: f32) -> vec4<f32> {
    return vec4<f32>(
        smoothstep(low, high, c.r),
        smoothstep(low, high, c.g),
        smoothstep(low, high, c.b),
        c.a,
    );
}
