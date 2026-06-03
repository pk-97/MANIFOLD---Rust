// node.distance_to_point — fusable body (freeze §12), SOURCE. Euclidean distance
// from (cx,cy), per-axis scaled then *scale, broadcast to RGB, A=1. Matches
// distance_to_point.wgsl. PARAMS: [cx, cy, scale, scale_x, scale_y].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32, scale: f32, scale_x: f32, scale_y: f32) -> vec4<f32> {
    let offset = (uv - vec2<f32>(cx, cy)) * vec2<f32>(scale_x, scale_y);
    let d = length(offset) * scale;
    return vec4<f32>(d, d, d, 1.0);
}
