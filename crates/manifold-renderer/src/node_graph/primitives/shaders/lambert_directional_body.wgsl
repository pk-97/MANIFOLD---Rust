// node.lambert_directional — fusable body (freeze §12). Coincident: the
// normal-map texel arrives as a register. This is the SCALAR-PARAM path only —
// light colour is the white default, so the hand kernel's `* light_color`
// multiply drops out (x * 1.0 is bit-identical). A wired `node.light` input is
// a non-scalar port, which the classify gate keeps as a boundary, so the fused
// path never sees the light-wire override. Same math as
// lambert_directional.wgsl otherwise. PARAMS: [light_x, light_y, light_z,
// ambient].
fn body(n_color: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, light_x: f32, light_y: f32, light_z: f32, ambient: f32) -> vec4<f32> {
    let n = n_color.rgb;
    let l = normalize(vec3<f32>(light_x, light_y, light_z) + vec3<f32>(1e-8));
    let lambert = max(dot(n, l), 0.0);
    let lit = lambert * (1.0 - ambient) + ambient;
    return vec4<f32>(vec3<f32>(lit), 1.0);
}
