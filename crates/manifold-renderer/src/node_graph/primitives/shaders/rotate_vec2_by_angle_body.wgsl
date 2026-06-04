// node.rotate_vec2_by_angle — fusable body (freeze §12), Pointwise. Rotate the
// input's RG vec2 by `angle` (radians): out.x = v.x*cos - v.y*sin, out.y = v.x*sin
// + v.y*cos, out = (.x, .y, 0, 1). The hand shader took CPU-precomputed cos_a/sin_a
// in its uniform; the body computes them from the `angle` param instead (the
// output is f16, so the sub-f16 GPU-vs-CPU trig difference is invisible). Matches
// rotate_vec2_by_angle.wgsl. PARAMS: [angle (Angle->f32)].
fn body(c_in: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, angle: f32) -> vec4<f32> {
    let v = c_in.rg;
    let cos_a = cos(angle);
    let sin_a = sin(angle);
    let r = vec2<f32>(
        v.x * cos_a - v.y * sin_a,
        v.x * sin_a + v.y * cos_a,
    );
    return vec4<f32>(r, 0.0, 1.0);
}
