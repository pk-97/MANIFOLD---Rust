// 3x3 box blur with multiplicative decay and subtractive evaporation.
// Used for trail diffusion — called 3 times per frame with different params.

struct DiffuseUniforms {
    decay: f32,
    sub_decay: f32,
    texel_x: f32,
    texel_y: f32,
};

@group(0) @binding(0) var<uniform> params: DiffuseUniforms;
@group(0) @binding(1) var t_trail: texture_2d<f32>;
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
    let tx = params.texel_x;
    let ty = params.texel_y;

    var sum: f32 = 0.0;
    sum += textureSample(t_trail, s, uv + vec2<f32>(-tx, -ty)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>( 0.0, -ty)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>( tx, -ty)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>(-tx,  0.0)).r;
    sum += textureSample(t_trail, s, uv).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>( tx,  0.0)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>(-tx,  ty)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>( 0.0,  ty)).r;
    sum += textureSample(t_trail, s, uv + vec2<f32>( tx,  ty)).r;

    let blurred = sum / 9.0;
    let result = max(0.0, blurred * params.decay - params.sub_decay);

    return vec4<f32>(result, 0.0, 0.0, 1.0);
}
