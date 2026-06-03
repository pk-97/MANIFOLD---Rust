// node.chroma_key — fusable body (freeze §12), Pointwise. Per-pixel colour
// proximity to `key_color` → soft mask; `invert` (the `mode` enum) flips
// Select/Reject. key_color is a Vec3 param: the codegen expands it to three
// uniform floats and hands the body a reassembled vec3<f32>. Matches
// chroma_key.wgsl. PARAMS: [key_color (Vec3), tolerance, softness, mode (Enum->u32)].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, key_color: vec3<f32>, tolerance: f32, softness: f32, invert: u32) -> vec4<f32> {
    let dist = length(c.rgb - key_color);
    let edge_lo = tolerance - softness;
    let edge_hi = tolerance + softness;
    let raw = 1.0 - smoothstep(edge_lo, edge_hi, dist);
    var mask = raw;
    if invert != 0u {
        mask = 1.0 - raw;
    }
    return vec4<f32>(mask, mask, mask, 1.0);
}
