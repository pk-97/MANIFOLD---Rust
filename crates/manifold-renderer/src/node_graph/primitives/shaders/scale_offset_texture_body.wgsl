// node.scale_offset_texture — fusable body (freeze §12). Per-pixel affine
// rgb*scale + offset, alpha pass-through. Pure own-texel. Matches
// scale_offset_texture.wgsl. PARAMS: [scale, offset].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32, offset: f32) -> vec4<f32> {
    return vec4<f32>(
        c.r * scale + offset,
        c.g * scale + offset,
        c.b * scale + offset,
        c.a,
    );
}
