// node.flash — fusable body (freeze §12). Brightness modulate by `amount` in
// three modes (0 Opacity toward black, 1 White, 2 Gain = 3x at amount=1). Pure
// own-texel. Matches flash.wgsl. PARAMS: [amount, mode]; mode is Enum -> u32.
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32, mode: u32) -> vec4<f32> {
    var col = c.rgb;
    if mode == 2u {
        col = col * mix(1.0, 3.0, amount);
    } else if mode == 1u {
        col = mix(col, vec3<f32>(1.0, 1.0, 1.0), amount);
    } else {
        col = col * (1.0 - amount);
    }
    return vec4<f32>(col, c.a);
}
