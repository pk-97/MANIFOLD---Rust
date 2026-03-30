// FluidParticleDisplay — mono tone mapping via extended Reinhard (fragment shader variant).

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    uv_scale: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
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
    let uv = (in.uv - vec2<f32>(0.5)) / max(params.uv_scale, 0.001) + vec2<f32>(0.5);

    let density = textureSample(t_density, s_density, uv).r;

    let x = density * params.intensity * params.contrast;
    let lum = x * (1.0 + x / 9.0) / (1.0 + x);

    return vec4<f32>(lum, lum, lum, lum);
}
