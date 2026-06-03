// node.box_mask — fusable body (freeze §12), SOURCE. Rotated rectangular SDF
// (Chebyshev distance in normalized half-extents), smoothstep falloff of width
// `softness`. Matches box_mask.wgsl. PARAMS: [cx, cy, half_width, half_height,
// rotation, softness].
fn body(uv: vec2<f32>, dims: vec2<f32>, cx: f32, cy: f32, half_width: f32, half_height: f32, rotation: f32, softness: f32) -> vec4<f32> {
    let p = uv - vec2<f32>(cx, cy);
    let c = cos(rotation);
    let s = sin(rotation);
    let p_rot = vec2<f32>(p.x * c + p.y * s, -p.x * s + p.y * c);
    let hw = max(half_width, 1e-6);
    let hh = max(half_height, 1e-6);
    let n_abs = abs(p_rot) / vec2<f32>(hw, hh);
    let dist = max(n_abs.x, n_abs.y);
    let soft = max(softness, 0.0);
    let edge_lo = max(1.0 - soft, 0.0);
    let edge_hi = 1.0 + soft;
    let mask = 1.0 - smoothstep(edge_lo, edge_hi, dist);
    return vec4<f32>(mask, mask, mask, 1.0);
}
