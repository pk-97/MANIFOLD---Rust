// node.blur_3d_separable — fusable body (freeze §12), 3D-VOLUME GATHER. Single-
// axis separable Gaussian blur on a Texture3D. `in` is gathered along one axis at
// the bilinear tap-pair midpoints (sigma = max(radius/2.5, 0.5)); the texel step
// comes from vol_res. The hand shader has two entry points (blur_scalar writes the
// blurred .r as (v,0,0,1); blur_vector writes the full blurred vec4) — they are
// merged here behind a runtime `mode` branch. The full vec4 accumulation's .r is
// bit-identical to the scalar accumulation (same per-component f32 ops), so the
// scalar branch just repacks. Matches generators/shaders/fluid_blur_3d.wgsl.
// PARAMS: [mode (Enum->u32), axis (Enum->u32), vol_res (Int->i32), radius].
fn body(src: texture_3d<f32>, samp: sampler, uv: vec3<f32>, dims: vec3<f32>, mode: u32, axis: u32, vol_res: i32, radius: f32) -> vec4<f32> {
    let sigma = max(radius / 2.5, 0.5);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
    let radius_int = i32(radius);
    let texel = 1.0 / f32(vol_res);

    var axis_dir = vec3<f32>(0.0);
    if axis == 0u { axis_dir.x = texel; }
    else if axis == 1u { axis_dir.y = texel; }
    else { axis_dir.z = texel; }

    var result = textureSampleLevel(src, samp, uv, 0.0);
    var total_weight = 1.0;

    var j: i32 = 1;
    loop {
        if j > radius_int { break; }

        let fj = f32(j);
        let w_a = exp(-(fj * fj) * inv_two_sigma_sq);

        if j + 1 <= radius_int {
            let fj1 = f32(j + 1);
            let w_b = exp(-(fj1 * fj1) * inv_two_sigma_sq);
            let w_ab = w_a + w_b;
            let offset = fj + w_b / w_ab;

            result += textureSampleLevel(src, samp, uv + axis_dir * offset, 0.0) * w_ab;
            result += textureSampleLevel(src, samp, uv - axis_dir * offset, 0.0) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            result += textureSampleLevel(src, samp, uv + axis_dir * fj, 0.0) * w_a;
            result += textureSampleLevel(src, samp, uv - axis_dir * fj, 0.0) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    let blurred = result / total_weight;
    if mode == 0u {
        // Scalar (density): write the blurred .r as (v, 0, 0, 1).
        return vec4<f32>(blurred.r, 0.0, 0.0, 1.0);
    }
    // Vector (force field): write the full blurred vec4.
    return blurred;
}
