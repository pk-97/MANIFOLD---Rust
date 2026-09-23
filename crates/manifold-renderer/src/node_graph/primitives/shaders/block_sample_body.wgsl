// node.block_sample — fusable body (GATHER). Keep the output at the source
// resolution while sampling the geometric centre of each integer pixel block.
// Positive columns/rows use normalized cells; zero keeps pixel units.
// The default linear sampler evaluates the block
// centre between texels for even-sized blocks; clamp the coordinate to the
// outermost source texel centre for partial blocks at the edges.
// PARAMS: [block_size, columns, rows].
fn body(source: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, block_size: f32, columns: f32, rows: f32) -> vec4<f32> {
    let group = max(round(block_size), 1.0);
    let size = vec2<f32>(
        select(group, dims.x * group / max(columns, 1.0), columns > 0.0),
        select(group, dims.y * group / max(rows, 1.0), rows > 0.0));
    let pixel = uv * dims;
    let block_origin = floor(pixel / size) * size;
    let centre = block_origin + 0.5 * size;
    let texel = vec2<f32>(0.5) / dims;
    let sample_uv = clamp(centre / dims, texel, vec2<f32>(1.0) - texel);
    return textureSampleLevel(source, samp, sample_uv, 0.0);
}
