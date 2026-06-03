// node.sharpen — fusable body (freeze §12), GATHER. 4-neighbour Laplacian
// unsharp mask: `in` is gathered at the four axis neighbours (one texel apart,
// texel = 1/dims) and the centre is sharpened by `amount`. amount<=0 returns the
// centre unchanged (the legacy passthrough fast-path). The hand shader derives
// the texel step from textureDimensions(src_tex); the body recovers the same
// step from the ambient `dims` (output == source size for a 1:1 filter). Matches
// sharpen.wgsl. PARAMS: [amount].
fn body(in_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, amount: f32) -> vec4<f32> {
    let center = textureSampleLevel(in_tex, samp, uv, 0.0);
    if amount <= 0.0 {
        return center;
    }
    let dx = vec2<f32>(1.0) / dims;
    let s_l = textureSampleLevel(in_tex, samp, uv + vec2<f32>(-dx.x, 0.0), 0.0);
    let s_r = textureSampleLevel(in_tex, samp, uv + vec2<f32>( dx.x, 0.0), 0.0);
    let s_u = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0, -dx.y), 0.0);
    let s_d = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0,  dx.y), 0.0);
    let laplacian = 4.0 * center - (s_l + s_r + s_u + s_d);
    return center + laplacian * amount * 0.5;
}
