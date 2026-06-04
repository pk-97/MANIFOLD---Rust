// node.normalize_vec2 — fusable body (freeze §12), paramless Pointwise. Safe-
// normalize of the input's RG vec2: out = (v/length(v), 0, 1) when length > eps,
// else (0, 0, 0, 1). Matches normalize_vec2.wgsl. PARAMS: [].
const NV_EPS: f32 = 1e-6;

fn body(c_in: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    let v = c_in.rg;
    let len = length(v);
    let n = select(vec2<f32>(0.0), v / len, len >= NV_EPS);
    return vec4<f32>(n, 0.0, 1.0);
}
