// node.gradient_map — fusable body (freeze §12). Maps input luminance to a
// two-stop gradient (color_a at luma 0 -> color_b at luma 1), preserving
// input coverage (premultiplied alpha in/out). Matches color_ramp.wgsl
// exactly. PARAMS: [color_a, color_b], each a Color param expanded to four
// consecutive f32 fields and reassembled as vec4<f32>.
fn body(
    c: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color_a: vec4<f32>,
    color_b: vec4<f32>,
) -> vec4<f32> {
    // Input is premultiplied alpha — unpremultiply to read the true colour
    // for the ramp index. A transparent pixel has no defined colour, so it
    // maps to luma 0 (and is masked back out below).
    let straight_rgb = select(vec3<f32>(0.0), c.rgb / max(c.a, 1e-4), c.a > 1e-4);
    let luma = clamp(dot(straight_rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    let ramp = mix(color_a, color_b, luma);
    // Preserve input coverage: a transparent input pixel stays transparent so
    // the gradient map keys over the layer below instead of painting color_a
    // as an opaque box. Output premultiplied (rgb * a).
    let out_a = c.a * ramp.a;
    return vec4<f32>(ramp.rgb * out_a, out_a);
}
