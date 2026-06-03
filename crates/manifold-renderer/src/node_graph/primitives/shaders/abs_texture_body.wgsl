// node.abs_texture — fusable body (freeze §12), PARAMLESS pointwise. abs(rgb),
// alpha pass-through. With no params the generated standalone kernel emits no
// uniform and no Params struct, so its textures start at binding 0 — matching
// abs_texture.wgsl exactly (the parity oracle).
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(abs(c.r), abs(c.g), abs(c.b), c.a);
}
