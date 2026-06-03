// node.clamp_texture — fusable body (freeze §12). Saturate RGB to [min, max].
// Pure; alpha passes through. Matches clamp_texture.wgsl. PARAMS order:
// [min, max] — args named min_v/max_v so they don't shadow the WGSL builtins
// (codegen passes params positionally, arg names are free).
fn body(c: vec4<f32>, min_v: f32, max_v: f32) -> vec4<f32> {
    return vec4<f32>(clamp(c.rgb, vec3<f32>(min_v), vec3<f32>(max_v)), c.a);
}
