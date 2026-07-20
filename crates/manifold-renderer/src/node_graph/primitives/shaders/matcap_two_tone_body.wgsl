// node.matcap_two_tone — fusable body (freeze §12). Cross-axis 4-colour
// matcap from a tangent-space normal map. Per pixel:
//   mc = n.xy * 0.5 + 0.5
//   base = mix(color_y_low, color_y_high, clamp(mc.y, 0, 1))
//   side = mix(color_x_low, color_x_high, clamp(mc.x, 0, 1))
//   out.rgb = (base + side) * 0.5; out.a = 1.0
//
// Matches matcap_two_tone.wgsl exactly. PARAMS order: [color_y_low,
// color_y_high, color_x_low, color_x_high] — all four Color params, each
// expanded to four consecutive f32 fields and reassembled as vec4<f32>.
fn body(
    c_normal: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color_y_low: vec4<f32>,
    color_y_high: vec4<f32>,
    color_x_low: vec4<f32>,
    color_x_high: vec4<f32>,
) -> vec4<f32> {
    let n = c_normal.rgb;
    let mc = n.xy * 0.5 + 0.5;
    let base = mix(color_y_low.rgb, color_y_high.rgb, clamp(mc.y, 0.0, 1.0));
    let side = mix(color_x_low.rgb, color_x_high.rgb, clamp(mc.x, 0.0, 1.0));
    let col = (base + side) * 0.5;
    return vec4<f32>(col, 1.0);
}
