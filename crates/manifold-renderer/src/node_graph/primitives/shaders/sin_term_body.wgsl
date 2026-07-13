// node.sine_wave — fusable body (freeze §12). Fused linear-projection + sin
// term: out = sin((a*field.r + b*field.g + c) * freq * freq_scale + time *
// time_scale), broadcast to RGB with A=1. Matches sin_term.wgsl exactly.
// First arg named `field` (not `c`) to avoid colliding with the `c` param
// (constant offset). PARAMS order: [a, b, c, freq, freq_scale, time, time_scale].
fn body(
    field: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    a: f32,
    b: f32,
    c: f32,
    freq: f32,
    freq_scale: f32,
    time: f32,
    time_scale: f32,
) -> vec4<f32> {
    let proj = a * field.r + b * field.g + c;
    let phase = time * time_scale;
    let fr = freq * freq_scale;
    let v = sin(proj * fr + phase);
    return vec4<f32>(v, v, v, 1.0);
}
