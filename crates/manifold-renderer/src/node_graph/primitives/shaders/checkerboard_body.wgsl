// node.checkerboard — fusable body (freeze §12), SOURCE (no texture input). A
// binary {0,1} checker from uv*scale + offset, broadcast to RGB, alpha 1. The
// body takes no colour arg — only the ambient uv/dims and its params. Matches
// checkerboard.wgsl. PARAMS: [scale, offset_x, offset_y].
fn body(uv: vec2<f32>, dims: vec2<f32>, scale: f32, offset_x: f32, offset_y: f32) -> vec4<f32> {
    let p = uv * scale + vec2<f32>(offset_x, offset_y);
    let ix = i32(floor(p.x));
    let iy = i32(floor(p.y));
    let on = ((ix + iy) & 1) == 0;
    let v = select(0.0, 1.0, on);
    return vec4<f32>(v, v, v, 1.0);
}
