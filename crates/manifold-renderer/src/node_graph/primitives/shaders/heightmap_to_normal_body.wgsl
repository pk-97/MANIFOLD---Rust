// node.heightmap_to_normal — fusable body (freeze §12), GATHER. Central-difference
// height (input.r) → unit normal. Samples the 4 axis neighbours (one texel apart,
// texel = 1/dims), builds the normal in TangentZ (coord_space 0) or WorldYUp
// (coord_space 1). Matches heightmap_to_normal.wgsl. PARAMS: [z_scale, aspect,
// coord_space (Enum->u32)].
fn body(tex_in: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, z_scale: f32, aspect: f32, coord_space: u32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;

    let hL = textureSampleLevel(tex_in, samp, uv + vec2<f32>(-inv.x, 0.0), 0.0).r;
    let hR = textureSampleLevel(tex_in, samp, uv + vec2<f32>( inv.x, 0.0), 0.0).r;
    let hD = textureSampleLevel(tex_in, samp, uv + vec2<f32>(0.0, -inv.y), 0.0).r;
    let hU = textureSampleLevel(tex_in, samp, uv + vec2<f32>(0.0,  inv.y), 0.0).r;
    let gx = (hR - hL) * 0.5;
    let gy = (hU - hD) * 0.5 * aspect;
    let z = max(z_scale, 1e-4);

    var n: vec3<f32>;
    if coord_space == 1u {
        n = normalize(vec3<f32>(-gx, z, -gy));
    } else {
        n = normalize(vec3<f32>(-gx, -gy, z));
    }
    return vec4<f32>(n, 1.0);
}
