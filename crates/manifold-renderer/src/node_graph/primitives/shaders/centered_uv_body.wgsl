// node.centered_uv — fusable body (freeze §12), SOURCE. UV recentered + per-axis
// scaled: R=(uv.x-cx)*scale_x, G=(uv.y-cy)*scale_y, B=0, A=1. Matches
// centered_uv.wgsl. PARAMS: [cx, cy, scale_x, scale_y].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32, scale_x: f32, scale_y: f32) -> vec4<f32> {
    let x = (uv.x - cx) * scale_x;
    let y = (uv.y - cy) * scale_y;
    return vec4<f32>(x, y, 0.0, 1.0);
}
