// node.radial_fold_uv — fusable body (freeze §12), SOURCE. Kaleidoscope fold:
// folds the plane into `segments` mirrored wedges around (cx,cy), emits the
// folded uv as R/G. TAU inlined. `segments` floors to >= 2. Matches
// radial_fold_uv.wgsl. PARAMS: [segments, cx, cy].
fn body(uv: vec2<f32>, dims: vec2<f32>, segments: f32, cx: f32, cy: f32) -> vec4<f32> {
    let centered = uv - vec2<f32>(cx, cy);
    let angle = atan2(centered.y, centered.x);
    let radius = length(centered);
    let seg = max(segments, 2.0);
    let segment_angle = 6.28318530717958647692 / seg;
    let slice_index = floor(angle / segment_angle);
    var local_angle = angle - slice_index * segment_angle;
    if (abs(slice_index) % 2.0) > 0.5 {
        local_angle = segment_angle - local_angle;
    }
    let kx = cos(local_angle) * radius + cx;
    let ky = sin(local_angle) * radius + cy;
    return vec4<f32>(kx, ky, 0.0, 1.0);
}
