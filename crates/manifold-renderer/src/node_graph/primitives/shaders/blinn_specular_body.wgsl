// node.shininess — fusable body (freeze §12). Blinn-Phong specular
// highlight from a tangent-space normal map. Per pixel:
//   h = normalize(light + view)
//   spec = pow(max(dot(n, h), 0), power)
//   out.rgb = color.rgb * spec; out.a = spec
//
// ADDITIVE specular term — sum with a base shading via `node.compose`
// mode=Add. Matches blinn_specular.wgsl exactly.
// PARAMS order: [light_x, light_y, light_z, view_x, view_y, view_z, power,
// color]. `color` is a Color param — the codegen expands it to four
// consecutive f32 fields and reassembles it as vec4<f32>.
fn body(
    c_normal: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    light_x: f32,
    light_y: f32,
    light_z: f32,
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    color: vec4<f32>,
) -> vec4<f32> {
    let n = c_normal.rgb;
    let l = normalize(vec3<f32>(light_x, light_y, light_z) + vec3<f32>(1e-8));
    let v = normalize(vec3<f32>(view_x, view_y, view_z) + vec3<f32>(1e-8));
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), max(power, 1e-4));
    return vec4<f32>(color.rgb * spec, spec);
}
