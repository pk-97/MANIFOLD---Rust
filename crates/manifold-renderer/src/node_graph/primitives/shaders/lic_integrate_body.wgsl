// node.lic_integrate — fusable body (freeze §12), 2-input GATHER (source +
// velocity both walked). Line Integral Convolution: walk N steps forward and
// backward along the normalised velocity field, weighted-accumulating source.r
// with a triangular weight. Both inputs are gathered along the streamline (the
// body computes each walker coord), so each arrives as a texture+sampler arg (the
// two samplers are the one shared sampler). steps is an Int param (i32 here),
// clamped to 64. Matches lic_integrate.wgsl. PARAMS: [steps (Int->i32), dt].
fn body(tex_source: texture_2d<f32>, s_source: sampler, tex_velocity: texture_2d<f32>, s_velocity: sampler, uv: vec2<f32>, dims: vec2<f32>, steps: i32, dt: f32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;
    let step_uv = dt * inv;
    let n = min(u32(steps), 64u);

    var sum: f32 = textureSampleLevel(tex_source, s_source, uv, 0.0).r;
    var w_total: f32 = 1.0;
    let inv_steps = 1.0 / f32(max(n, 1u));

    var walker = uv;
    for (var i: u32 = 1u; i <= n; i = i + 1u) {
        let v = textureSampleLevel(tex_velocity, s_velocity, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker + vn * step_uv;
        let w = 1.0 - f32(i) * inv_steps;
        sum = sum + textureSampleLevel(tex_source, s_source, walker, 0.0).r * w;
        w_total = w_total + w;
    }

    walker = uv;
    for (var i: u32 = 1u; i <= n; i = i + 1u) {
        let v = textureSampleLevel(tex_velocity, s_velocity, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker - vn * step_uv;
        let w = 1.0 - f32(i) * inv_steps;
        sum = sum + textureSampleLevel(tex_source, s_source, walker, 0.0).r * w;
        w_total = w_total + w;
    }

    let acc = sum / max(w_total, 1e-4);
    return vec4<f32>(acc, 0.0, 0.0, 1.0);
}
