// node.reinhard_tone_map — fusable body (freeze §12). Reinhard tone map in two
// curves (0 Extended x*(1+x/9)/(1+x), 1 Simple x/(x+1)) after an intensity*
// contrast pre-multiply; alpha pass-through. Pure own-texel. Matches
// reinhard_tone_map.wgsl. PARAMS: [intensity, contrast, curve]; curve is Enum -> u32.
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, intensity: f32, contrast: f32, curve: u32) -> vec4<f32> {
    let x = c.rgb * intensity * contrast;
    var mapped: vec3<f32>;
    if curve == 1u {
        mapped = x / (x + vec3<f32>(1.0));
    } else {
        mapped = x * (1.0 + x / vec3<f32>(9.0)) / (1.0 + x);
    }
    return vec4<f32>(mapped, c.a);
}
