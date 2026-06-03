// node.texture_sum_5 — fusable body (freeze §12), MultiInputCoincident: five
// inputs at the SAME uv. (a+b+c+d+e) * select(1/divisor, 0, |divisor|<1e-9)
// (divide-by-zero clamps to 0). Matches texture_sum_5.wgsl. PARAMS: [divisor].
fn body(a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32>, e: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, divisor: f32) -> vec4<f32> {
    let s = a + b + c + d + e;
    let inv = select(1.0 / divisor, 0.0, abs(divisor) < 1e-9);
    return s * inv;
}
