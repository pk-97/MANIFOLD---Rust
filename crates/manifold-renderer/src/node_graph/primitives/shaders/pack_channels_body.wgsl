// node.pack_channels — fusable body (freeze §12), 4-input Coincident with
// OPTIONAL-INPUT use-flags. Reads the .r of each of r/g/b/a (sampled at uv) into
// the matching output channel; when an input is unwired (use_*==0) that channel
// falls back to default_*. The codegen injects a use_<name> flag per optional
// input (run() packs input.is_some()); unwired inputs bind a dummy texture, so the
// pre-read c_* is harmlessly discarded. Matches pack_channels.wgsl. PARAMS:
// [default_r, default_g, default_b, default_a] + injected use_r/g/b/a.
fn body(c_r: vec4<f32>, c_g: vec4<f32>, c_b: vec4<f32>, c_a: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, default_r: f32, default_g: f32, default_b: f32, default_a: f32, use_r: u32, use_g: u32, use_b: u32, use_a: u32) -> vec4<f32> {
    var rgba = vec4<f32>(default_r, default_g, default_b, default_a);
    if use_r != 0u { rgba.r = c_r.r; }
    if use_g != 0u { rgba.g = c_g.r; }
    if use_b != 0u { rgba.b = c_b.r; }
    if use_a != 0u { rgba.a = c_a.r; }
    return rgba;
}
