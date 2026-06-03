// node.dither — fusable body (freeze §12). The first CoincidentTexel atom: both
// inputs are read by EXACT integer texel (textureLoad, no sampler) because the
// `pattern` input is an ordered threshold map where each texel is a distinct
// value — sampling would blend neighbouring thresholds and smear the dither.
// Both inputs MUST match the output resolution (the region-grower enforces this
// for CoincidentTexel inputs). Quantize Rec.709 luma to 8→2 levels (by amount),
// dither-biased by the pattern's R channel, preserve hue by scaling the original
// colour by the dithered/original luma ratio, crossfade against the source.
// Verbatim from dither.wgsl (the parity oracle). PARAMS order: [amount].
fn body(c: vec4<f32>, pattern: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32) -> vec4<f32> {
    let original = c.rgb;
    let threshold = pattern.r;

    let lum = dot(original, vec3<f32>(0.2126, 0.7152, 0.0722));
    let levels = mix(8.0, 2.0, amount);

    var dithered = (lum + (threshold - 0.5) / levels) * levels;
    dithered = floor(dithered + 0.5) / levels;
    dithered = clamp(dithered, 0.0, 1.0);

    var scale: f32;
    if lum > 0.001 {
        scale = dithered / lum;
    } else {
        scale = dithered;
    }
    let dithered_color = original * scale;

    let result = mix(original, dithered_color, amount);
    return vec4<f32>(result, c.a);
}
