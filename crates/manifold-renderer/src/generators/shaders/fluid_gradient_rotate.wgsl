// Gradient + rotation pass: compute central-difference gradient of blurred
// density field, scale by flow, rotate by curl angle to produce 2D force field.

struct GradientUniforms {
    texel_x: f32,
    texel_y: f32,
    slope_strength: f32,
    curl_angle_rad: f32,
};

@group(0) @binding(0) var<uniform> params: GradientUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var s_density: sampler;

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
    let texel = vec2<f32>(params.texel_x, params.texel_y);

    // Central differences on blurred density
    let dR = textureSample(t_density, s_density, uv + vec2<f32>(texel.x, 0.0)).r;
    let dL = textureSample(t_density, s_density, uv - vec2<f32>(texel.x, 0.0)).r;
    let dU = textureSample(t_density, s_density, uv + vec2<f32>(0.0, texel.y)).r;
    let dD = textureSample(t_density, s_density, uv - vec2<f32>(0.0, texel.y)).r;

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
