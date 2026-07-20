// node.rim_light — fusable body (freeze §12). Fresnel-based edge highlight
// from a tangent-space normal map. Per pixel:
//   f = pow(1 - max(dot(n, view), 0), power)
//   out.rgb = color.rgb * f; out.a = f
//
// ADDITIVE rim term — black at face-on, `color`-tinted at grazing. Matches
// fresnel_rim.wgsl exactly. PARAMS order: [view_x, view_y, view_z, power,
// color]. `color` is a Color param — expands to four consecutive f32
// fields, reassembled as vec4<f32>.
fn body(
    c_normal: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    color: vec4<f32>,
) -> vec4<f32> {
    let n = c_normal.rgb;
    let v = normalize(vec3<f32>(view_x, view_y, view_z) + vec3<f32>(1e-8));
    let face = max(dot(n, v), 0.0);
    let f = pow(1.0 - face, max(power, 1e-4));
    return vec4<f32>(color.rgb * f, f);
}
