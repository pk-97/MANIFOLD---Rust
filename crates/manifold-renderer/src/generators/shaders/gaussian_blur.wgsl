// Separable Gaussian blur with bilinear tap pairing.
// Pairs adjacent integer offsets (j, j+1) into a single weighted-midpoint
// fetch, halving the sample count. Called twice per blur (H then V).
// Unity ref: Assets/Shaders/GaussianBlur.shader

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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel_step = params.direction * vec2<f32>(params.texel_x, params.texel_y);
    let sigma = max(params.radius / 3.0, 1.0);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    // Center tap
    var result = textureSample(t_source, s_source, uv);
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

            result += textureSample(t_source, s_source, uv + texel_step * offset) * w_ab;
            result += textureSample(t_source, s_source, uv - texel_step * offset) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            // Unpaired last tap (odd radius)
            result += textureSample(t_source, s_source, uv + texel_step * fj) * w_a;
            result += textureSample(t_source, s_source, uv - texel_step * fj) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    return result / total_weight;
}
