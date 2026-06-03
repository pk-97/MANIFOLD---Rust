// node.field_combine — fusable body (freeze §12). Per-pixel scalar field
// a*in.r + b*in.g + c, broadcast to RGB, alpha forced to 1. Pure own-texel. The
// colour arg is named `s` (not `c`) so it doesn't clash with the `c` param.
// Matches field_combine.wgsl. PARAMS: [a, b, c].
fn body(s: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, a: f32, b: f32, c: f32) -> vec4<f32> {
    let v = a * s.r + b * s.g + c;
    return vec4<f32>(v, v, v, 1.0);
}
