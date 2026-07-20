// node.channel_mixer — fusable body (freeze §12). Per-pixel 4x4 RGBA
// matrix transform: out = M . in, M's rows are the four Vec4 params.
// Matches channel_mix.wgsl exactly. PARAMS: [row0, row1, row2, row3], each
// a Vec4 param expanded to four consecutive f32 fields and reassembled as
// vec4<f32>.
fn body(
    c: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    row3: vec4<f32>,
) -> vec4<f32> {
    return vec4<f32>(dot(row0, c), dot(row1, c), dot(row2, c), dot(row3, c));
}
