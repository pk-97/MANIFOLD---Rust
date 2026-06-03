// node.contrast — fusable body (freeze §12). Pivot-around-0.5 contrast,
// HDR-safe (no clamp). Pure; alpha passes through. Matches contrast.wgsl.
// PARAMS order: [contrast].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, contrast: f32) -> vec4<f32> {
    return vec4<f32>((c.rgb - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5), c.a);
}
