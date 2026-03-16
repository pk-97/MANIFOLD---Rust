// Separable Gaussian blur with discrete texel taps.
// Called twice per blur operation (H then V), used for both
// density blur and vector field blur.
// Uses textureLoad (not textureSample) because R32Float/Rg32Float
// are not filterable on Metal.

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

fn load_clamped(uv: vec2<f32>, dims: vec2<u32>) -> vec4<f32> {
    let coord = clamp(
        vec2<i32>(vec2<f32>(dims) * uv),
        vec2<i32>(0, 0),
        vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1),
    );
    return textureLoad(t_source, coord, 0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let dims = textureDimensions(t_source);
    let texel_size = vec2<f32>(params.texel_x, params.texel_y);
    let sigma = max(params.radius / 3.0, 1.0);
    let two_sigma_sq = 2.0 * sigma * sigma;

    // Center tap
    var center_weight = 1.0;
    var result = load_clamped(uv, dims) * center_weight;
    var total_weight = center_weight;

    // Gaussian taps at integer offsets
    let radius_int = i32(params.radius);
    for (var j: i32 = 1; j <= radius_int; j = j + 1) {
        let fj = f32(j);
        let w = exp(-(fj * fj) / two_sigma_sq);
        let sample_offset = params.direction * fj * texel_size;
        result += load_clamped(uv + sample_offset, dims) * w;
        result += load_clamped(uv - sample_offset, dims) * w;
        total_weight += w * 2.0;
    }

    return result / total_weight;
}
