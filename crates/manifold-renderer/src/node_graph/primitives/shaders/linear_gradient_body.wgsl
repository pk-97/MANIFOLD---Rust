// node.linear_gradient — fusable body (freeze §12), SOURCE. Directional 0→1 ramp
// smoothstepped across a line through (cx,cy) perpendicular to `rotation`, band
// width `softness`. Broadcast to RGB, A=1. Matches linear_gradient.wgsl. PARAMS:
// [cx, cy, rotation, softness].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32, rotation: f32, softness: f32) -> vec4<f32> {
    let p = uv - vec2<f32>(cx, cy);
    let dir = vec2<f32>(cos(rotation), sin(rotation));
    let t = dot(p, dir);
    let half_soft = max(softness * 0.5, 1e-6);
    let mask = smoothstep(-half_soft, half_soft, t);
    return vec4<f32>(mask, mask, mask, 1.0);
}
