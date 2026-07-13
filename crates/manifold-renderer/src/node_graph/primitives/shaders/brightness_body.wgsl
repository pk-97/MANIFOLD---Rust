// node.brightness — fusable body (freeze §12). RGB -> weighted grayscale
// (luma) via per-channel weights. Matches brightness.wgsl exactly.
// PARAMS: [weights (Vec3 -> vec3<f32>)].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, weights: vec3<f32>) -> vec4<f32> {
    let g = dot(c.rgb, weights);
    return vec4<f32>(g, g, g, c.a);
}
