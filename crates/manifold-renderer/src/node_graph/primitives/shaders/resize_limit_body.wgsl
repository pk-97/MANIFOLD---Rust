// node.resize_limit — bilinear resampling body.
//
// `output_dims` has already applied the longest-dimension cap, so the output
// grid is never larger than the source grid. Gather keeps the source texture
// and sampler visible to this body when the atom is fused or standalone.
fn body(source: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, _max_dim: i32) -> vec4<f32> {
    let source_dims = vec2<f32>(textureDimensions(source));
    let source_texel = 0.5 / source_dims;
    let sample_uv = clamp(uv, source_texel, vec2<f32>(1.0) - source_texel);
    return textureSampleLevel(source, samp, sample_uv, 0.0);
}
