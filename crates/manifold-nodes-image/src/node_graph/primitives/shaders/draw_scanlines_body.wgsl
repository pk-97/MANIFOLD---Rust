// `node.draw_scanlines` fusable body (D3, BUG-114). No array input — this
// atom is a plain single-texture Pointwise op; it only needed the P5
// Color-param lift (`classify_node`'s param gate) to become fusable, not the
// BufferIndex mechanism. Matches `draw_scanlines.wgsl`'s math verbatim.
fn body(
    c_in: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color: vec4<f32>,
    alpha: f32,
    period_px: f32,
    intensity: f32,
) -> vec4<f32> {
    let scanline = abs(fract(uv.y * dims.y / period_px) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * intensity;

    let add = scan_alpha * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
