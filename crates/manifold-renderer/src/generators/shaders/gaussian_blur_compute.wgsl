// Compute variant of gaussian_blur.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel (already used in fragment) stays the same
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct BlurUniforms {
    direction: vec2<f32>,
    radius: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> params: BlurUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let texel_step = params.direction * vec2<f32>(params.texel_x, params.texel_y);
    let sigma = max(params.radius / 3.0, 1.0);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    // Center tap
    var result = textureSampleLevel(t_source, s_source, uv, 0.0);
    var total_weight = 1.0;

    // Bilinear tap trick: pair adjacent samples (j, j+1) into a single
    // bilinear-filtered fetch at their weighted midpoint. Hardware
    // interpolation computes the same weighted sum — halves sample count.
    let radius_int = i32(params.radius);
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

            result += textureSampleLevel(t_source, s_source, uv + texel_step * offset, 0.0) * w_ab;
            result += textureSampleLevel(t_source, s_source, uv - texel_step * offset, 0.0) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            // Unpaired last tap (odd radius)
            result += textureSampleLevel(t_source, s_source, uv + texel_step * fj, 0.0) * w_a;
            result += textureSampleLevel(t_source, s_source, uv - texel_step * fj, 0.0) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    textureStore(output_tex, vec2<i32>(gid.xy), result / total_weight);
}
