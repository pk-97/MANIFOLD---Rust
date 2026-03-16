// Separable Gaussian blur with bilinear tap optimization.
// Called twice per blur operation (H then V), used for both
// density blur and vector field blur.

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
@group(0) @binding(2) var s: sampler;

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
    let texel_size = vec2<f32>(params.texel_x, params.texel_y);
    let sigma = max(params.radius / 3.0, 1.0);
    let two_sigma_sq = 2.0 * sigma * sigma;

    // Center tap
    var center_weight = 1.0;
    var result = textureSample(t_source, s, uv) * center_weight;
    var total_weight = center_weight;

    // Bilinear taps: pair adjacent offsets into single fetches
    let radius_int = i32(params.radius);
    for (var j: i32 = 1; j < radius_int; j = j + 2) {
        let fj = f32(j);
        let fj1 = fj + 1.0;
        let wA = exp(-(fj * fj) / two_sigma_sq);
        let wB = exp(-(fj1 * fj1) / two_sigma_sq);
        let combined = wA + wB;
        let offset = fj + wB / combined;

        let sample_offset = params.direction * offset * texel_size;
        result += textureSample(t_source, s, uv + sample_offset) * combined;
        result += textureSample(t_source, s, uv - sample_offset) * combined;
        total_weight += combined * 2.0;
    }

    // Handle odd final tap if radius is even
    if radius_int > 1 && radius_int % 2 == 0 {
        let fj = f32(radius_int - 1);
        let w = exp(-(fj * fj) / two_sigma_sq);
        let sample_offset = params.direction * fj * texel_size;
        result += textureSample(t_source, s, uv + sample_offset) * w;
        result += textureSample(t_source, s, uv - sample_offset) * w;
        total_weight += w * 2.0;
    }

    return result / total_weight;
}
