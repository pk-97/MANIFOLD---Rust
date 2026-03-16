struct Uniforms {
    beat: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) alpha: f32,
    @location(3) _pad: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

// Orthographic projection: [0,1] -> [-1,1] NDC
@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc_x = in.position.x * 2.0 - 1.0;
    let ndc_y = in.position.y * 2.0 - 1.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.alpha = in.alpha;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dist = length(in.uv);
    let aa = 1.0 - smoothstep(0.85, 1.0, dist);
    let beat_frac = fract(u.beat);
    let flash = smoothstep(0.1, 0.0, beat_frac) * 0.4;
    let lum = clamp(aa + flash * aa, 0.0, 1.0) * in.alpha;
    return vec4<f32>(lum, lum, lum, lum);
}
