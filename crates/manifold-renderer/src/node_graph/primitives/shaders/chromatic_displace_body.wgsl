// node.chromatic_displace — fusable body (freeze §12), GATHER. `in` is gathered
// at three per-channel offset taps along the `velocity` field (R at uv-off, G at
// centre, B at uv+off); `velocity` is coincident. off = velocity.rg * amount /
// dims. Matches chromatic_displace.wgsl. PARAMS: [amount].
fn body(in_tex: texture_2d<f32>, samp: sampler, velocity: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;
    let v = velocity.rg;
    let off = v * amount * inv;
    let s_r = textureSampleLevel(in_tex, samp, uv - off, 0.0).r;
    let s_c = textureSampleLevel(in_tex, samp, uv, 0.0);
    let s_b = textureSampleLevel(in_tex, samp, uv + off, 0.0).b;
    return vec4<f32>(s_r, s_c.g, s_b, s_c.a);
}
