// EdgeStretch effect — clamps UVs to a center strip, stretching edge pixels.

struct Uniforms {
    amount: f32,
    source_width: f32,  // 0.1..0.9 — width of the visible center strip
    mode: u32,          // 0=Horizontal, 1=Vertical, 2=Both
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);
    let original = src.rgb;

    let half_width = uniforms.source_width * 0.5;
    let left_edge = 0.5 - half_width;
    let right_edge = 0.5 + half_width;

    var stretch_uv = in.uv;

    // 0 = Horizontal, 1 = Vertical, 2 = Both
    if uniforms.mode == 0u || uniforms.mode == 2u {
        stretch_uv.x = clamp(stretch_uv.x, left_edge, right_edge);
    }
    if uniforms.mode == 1u || uniforms.mode == 2u {
        stretch_uv.y = clamp(stretch_uv.y, left_edge, right_edge);
    }

    let stretch_sample = textureSample(source_tex, tex_sampler, stretch_uv);
    let result = mix(original, stretch_sample.rgb, uniforms.amount);
    return vec4<f32>(result, mix(src.a, stretch_sample.a, uniforms.amount));
}
