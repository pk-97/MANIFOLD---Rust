// node.wet_dry — fusable body (freeze §12), MultiInputCoincident: dry + wet
// sampled at the SAME uv. Full-RGBA lerp dry->wet by wet_dry. Matches
// wet_dry_mix.wgsl. PARAMS: [wet_dry].
fn body(dry: vec4<f32>, wet: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, wet_dry: f32) -> vec4<f32> {
    return mix(dry, wet, wet_dry);
}
