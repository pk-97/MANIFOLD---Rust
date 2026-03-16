struct Uniforms {
    uv_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;

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
    // Center and apply scale
    let uv = (in.uv - vec2<f32>(0.5)) * u.uv_scale + vec2<f32>(0.5);
    let c = textureSample(state_tex, state_sampler, uv);
    let b_val = c.g;

    var lum = smoothstep(0.0, 0.4, b_val);

    // Edge highlight via screen-space derivatives
    let ddx_b = dpdx(b_val);
    let ddy_b = dpdy(b_val);
    let edge = length(vec2<f32>(ddx_b, ddy_b)) * 40.0;
    lum += edge * 0.2;

    return vec4<f32>(lum, lum, lum, lum);
}
