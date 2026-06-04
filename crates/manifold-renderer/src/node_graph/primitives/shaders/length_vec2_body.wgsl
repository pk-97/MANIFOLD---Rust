// node.length_vec2 — fusable body (freeze §12), paramless Pointwise. Per-pixel
// scalar magnitude of the input's RG vec2: out = (length(in.rg), 0, 0, 1). Matches
// length_vec2.wgsl. PARAMS: [].
fn body(c_in: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    let v = c_in.rg;
    return vec4<f32>(length(v), 0.0, 0.0, 1.0);
}
