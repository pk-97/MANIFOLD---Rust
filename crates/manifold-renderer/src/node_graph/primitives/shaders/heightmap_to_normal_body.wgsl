// node.heightmap_to_normal — fusable body (freeze §12), GATHER_TEXEL. Central-difference
// height (input.r) → unit normal. Reads the 4 axis neighbours via an EXACT
// integer textureLoad (one texel index step, clamped to the texture bounds —
// manual ClampToEdge). D6(a) (docs/DEPTH_RELIGHT_DESIGN.md): converted from a
// filtering-sampler Gather read so `in` can carry `precision_critical` — every
// offset here lands on an exact texel center (uv = (id+0.5)/dims, offset by
// exactly ±1 texel), so textureLoad+clamp agrees with the old
// textureSampleLevel+ClampToEdge sampler bit-for-bit (proven by
// gpu_tests::gather_texel_conversion_is_value_preserving). Builds the normal
// in TangentZ (coord_space 0) or WorldYUp (coord_space 1). Matches
// heightmap_to_normal.wgsl. PARAMS: [z_scale, aspect, coord_space (Enum->u32)].
fn body(tex_in: texture_2d<f32>, uv: vec2<f32>, dims: vec2<f32>, z_scale: f32, aspect: f32, coord_space: u32) -> vec4<f32> {
    let dims_i = vec2<i32>(dims);
    let max_c = dims_i - vec2<i32>(1, 1);
    let c = vec2<i32>(uv * dims);

    let cL = clamp(c - vec2<i32>(1, 0), vec2<i32>(0, 0), max_c);
    let cR = clamp(c + vec2<i32>(1, 0), vec2<i32>(0, 0), max_c);
    let cD = clamp(c - vec2<i32>(0, 1), vec2<i32>(0, 0), max_c);
    let cU = clamp(c + vec2<i32>(0, 1), vec2<i32>(0, 0), max_c);

    let hL = textureLoad(tex_in, cL, 0).r;
    let hR = textureLoad(tex_in, cR, 0).r;
    let hD = textureLoad(tex_in, cD, 0).r;
    let hU = textureLoad(tex_in, cU, 0).r;
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
