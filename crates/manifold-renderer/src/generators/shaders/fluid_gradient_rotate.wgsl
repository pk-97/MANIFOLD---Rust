// Gradient + rotation pass: compute central-difference gradient of blurred
// density field, scale by flow, rotate by curl angle to produce 2D force field.
// Uses textureLoad (not textureSample) because R32Float is not filterable on Metal.

struct GradientUniforms {
    texel_x: f32,
    texel_y: f32,
    slope_strength: f32,
    curl_angle_rad: f32,
};

@group(0) @binding(0) var<uniform> params: GradientUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;

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

fn load_density(uv: vec2<f32>, dims: vec2<u32>) -> f32 {
    let coord = clamp(
        vec2<i32>(vec2<f32>(dims) * uv),
        vec2<i32>(0, 0),
        vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1),
    );
    return textureLoad(t_density, coord, 0).r;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let dims = textureDimensions(t_density);
    let texel = vec2<f32>(params.texel_x, params.texel_y);

    // Central differences on blurred density
    let dR = load_density(uv + vec2<f32>(texel.x, 0.0), dims);
    let dL = load_density(uv - vec2<f32>(texel.x, 0.0), dims);
    let dU = load_density(uv + vec2<f32>(0.0, texel.y), dims);
    let dD = load_density(uv - vec2<f32>(0.0, texel.y), dims);

    let grad = vec2<f32>(dR - dL, dU - dD) / (2.0 * texel);
    let scaled = grad * params.slope_strength;

    // Rotate by curl angle
    let cos_r = cos(params.curl_angle_rad);
    let sin_r = sin(params.curl_angle_rad);
    let force = vec2<f32>(
        scaled.x * cos_r - scaled.y * sin_r,
        scaled.x * sin_r + scaled.y * cos_r,
    );

    return vec4<f32>(force.x, force.y, 0.0, 1.0);
}
