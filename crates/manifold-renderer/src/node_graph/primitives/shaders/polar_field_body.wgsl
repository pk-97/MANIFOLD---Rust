// node.polar_field — fusable body (freeze §12), SOURCE. R=angle (atan2
// normalized to 0..1), G=radius (UV distance), B=0, A=1. PI/TAU inlined as
// literals (matches polar_field.wgsl's consts) to avoid any fused-region const
// collision. PARAMS: [cx, cy].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32) -> vec4<f32> {
    let d = uv - vec2<f32>(cx, cy);
    let angle = (atan2(d.y, d.x) + 3.14159265358979323846) / 6.28318530717958647692;
    let radius = length(d);
    return vec4<f32>(angle, radius, 0.0, 1.0);
}
