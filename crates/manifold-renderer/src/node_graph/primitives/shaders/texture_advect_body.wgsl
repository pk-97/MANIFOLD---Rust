// node.texture_advect — fusable body (freeze §12), 2-input gather (in advected,
// velocity coincident). Backward semi-Lagrangian advection: sample `in` at uv -
// velocity.rg * dt / dims. `in` is gathered (the body computes adv_uv), `velocity`
// is coincident (pre-sampled at uv). The `boundary` param selects the sampler wrap
// mode host-side (Repeat/Clamp), so the body ignores it. Matches texture_advect
// .wgsl. PARAMS: [dt, boundary (Enum->u32, host-side sampler)].
fn body(tex_in: texture_2d<f32>, samp: sampler, c_velocity: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, dt: f32, boundary: u32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;
    let v = c_velocity.rg;
    let adv_uv = uv - v * dt * inv;
    return textureSampleLevel(tex_in, samp, adv_uv, 0.0);
}
