// Wet/dry lerp shader for effect group blending.
// Matches Unity's GroupWetDryLerp.shader: lerp(dry, wet, _WetDry)

struct Uniforms {
    wet_dry: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_dry: texture_2d<f32>;
@group(0) @binding(2) var t_wet: texture_2d<f32>;
@group(0) @binding(3) var s: sampler;

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
    let dry = textureSample(t_dry, s, in.uv);
    let wet = textureSample(t_wet, s, in.uv);
    return mix(dry, wet, u.wet_dry);
}
