// Feedback effect — lerps current frame with previous frame's state.
// Creates trailing/ghosting visual persistence.

struct Uniforms {
    feedback_amount: f32,  // 0..1 — how much of previous frame to retain
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;       // current frame
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var feedback_tex: texture_2d<f32>;     // previous frame state

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
    let current = textureSample(source_tex, tex_sampler, in.uv);
    let previous = textureSample(feedback_tex, tex_sampler, in.uv);
    return mix(current, previous, uniforms.feedback_amount);
}
