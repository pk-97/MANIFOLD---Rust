// node.uv_displace_by_flow — fusable body (freeze §12), GATHER. `in` is gathered
// at uv + offset where offset = (flow.rb - bias) * weight; `flow` is coincident.
// Matches uv_displace_by_flow.wgsl. PARAMS: [weight, bias].
fn body(source: texture_2d<f32>, samp: sampler, flow: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, weight: f32, bias: f32) -> vec4<f32> {
    let offset = (vec2<f32>(flow.r, flow.b) - vec2<f32>(bias)) * weight;
    let sampled_uv = uv + offset;
    return textureSampleLevel(source, samp, sampled_uv, 0.0);
}
