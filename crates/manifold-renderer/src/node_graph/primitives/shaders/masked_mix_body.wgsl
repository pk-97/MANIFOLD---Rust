// node.masked_mix — fusable body (freeze §12), MultiInputCoincident: a, b, mask
// sampled at the SAME uv. weight = clamp(mask.r * amount, 0, 1); mix(a, b,
// weight). Matches masked_mix.wgsl. PARAMS: [amount].
fn body(a: vec4<f32>, b: vec4<f32>, mask: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32) -> vec4<f32> {
    let weight = clamp(mask.r * amount, 0.0, 1.0);
    return mix(a, b, weight);
}
