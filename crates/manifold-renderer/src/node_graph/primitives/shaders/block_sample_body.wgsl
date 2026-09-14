// node.block_sample — fusable body (GATHER). Keep the output at the source
// resolution while sampling the geometric centre of each integer pixel block.
// `uv` is the current output pixel centre, so floor(uv*dims) recovers its
// integer pixel coordinate without relying on exact half-texel subtraction.
// The default linear sampler evaluates the block
// centre between texels for even-sized blocks; clamp the coordinate to the
// outermost source texel centre for partial blocks at the edges.
// PARAMS: [block_size].
fn body(source: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, block_size: f32) -> vec4<f32> {
    let size = max(round(block_size), 1.0);
    let pixel = floor(uv * dims);
    let block_origin = floor(pixel / size) * size;
    let centre = block_origin + vec2<f32>(0.5 * size);
    let texel = vec2<f32>(0.5) / dims;
    let sample_uv = clamp(centre / dims, texel, vec2<f32>(1.0) - texel);
    return textureSampleLevel(source, samp, sample_uv, 0.0);
}
