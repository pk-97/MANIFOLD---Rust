// node.uv_field — fusable body (freeze §12), SOURCE (no texture input, no
// params). Emits the fragment uv as R/G, B=0, A=1. The foundation coordinate
// generator. Matches uv_field.wgsl.
fn body(uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(uv.x, uv.y, 0.0, 1.0);
}
