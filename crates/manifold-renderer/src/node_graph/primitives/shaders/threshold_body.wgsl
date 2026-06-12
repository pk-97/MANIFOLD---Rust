// node.threshold — fusable body (freeze §12). Coincident: the source colour
// arrives as a register. Verbatim port of the soft-knee bright-pass response
// from threshold.wgsl (the legacy bloom prefilter curve). PARAMS: [level,
// softness].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, level: f32, softness: f32) -> vec4<f32> {
    let lum = max(c.r, max(c.g, c.b));
    let soft_start = level - softness;
    var t = clamp((lum - soft_start) / max(2.0 * softness, 1e-5), 0.0, 1.0);
    t = t * t * (3.0 - 2.0 * t);
    let hard = clamp((lum - level) / max(1.0 - level, 1e-5), 0.0, 1.0);
    let response = max(t * 0.78, hard);
    return vec4<f32>(c.rgb * response, c.a);
}
