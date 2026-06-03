// node.ellipse_mask — fusable body (freeze §12), SOURCE. Rotated elliptical SDF
// mask, smoothstep falloff of width `softness`. Matches ellipse_mask.wgsl.
// PARAMS: [cx, cy, radius_x, radius_y, rotation, softness].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32, radius_x: f32, radius_y: f32, rotation: f32, softness: f32) -> vec4<f32> {
    let p = uv - vec2<f32>(cx, cy);
    let c = cos(rotation);
    let s = sin(rotation);
    let p_rot = vec2<f32>(p.x * c + p.y * s, -p.x * s + p.y * c);
    let rx = max(radius_x, 1e-6);
    let ry = max(radius_y, 1e-6);
    let n = p_rot / vec2<f32>(rx, ry);
    let dist = length(n);
    let soft = max(softness, 0.0);
    let edge_lo = max(1.0 - soft, 0.0);
    let edge_hi = 1.0 + soft;
    let mask = 1.0 - smoothstep(edge_lo, edge_hi, dist);
    return vec4<f32>(mask, mask, mask, 1.0);
}
