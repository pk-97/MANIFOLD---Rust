// node.gradient_central_diff — fusable body (freeze §12), GATHER. Per-pixel
// central-difference gradient of one channel of `in`: samples the 4 axis
// neighbours (one texel apart, texel = 1/dims recovered from the ambient dims),
// outputs (dx, dy, 0, 1). scale_mode 0 = Texel (×0.5), 1 = UV (×dim*0.5 per
// axis). wrap_mode selects the sampler (Clamp/Repeat) host-side, so the body
// ignores it. Matches gradient_central_diff.wgsl. PARAMS: [channel (Enum->u32),
// scale_mode (Enum->u32), wrap_mode (Enum->u32, host-side sampler)].
fn gcd_select_channel(c: vec4<f32>, idx: u32) -> f32 {
    switch idx {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        default: { return c.a; }
    }
}

fn body(in_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, channel: u32, scale_mode: u32, wrap_mode: u32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;

    let cL = textureSampleLevel(in_tex, samp, uv + vec2<f32>(-inv.x, 0.0), 0.0);
    let cR = textureSampleLevel(in_tex, samp, uv + vec2<f32>( inv.x, 0.0), 0.0);
    let cD = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0, -inv.y), 0.0);
    let cU = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0,  inv.y), 0.0);

    let diff_x = gcd_select_channel(cR, channel) - gcd_select_channel(cL, channel);
    let diff_y = gcd_select_channel(cU, channel) - gcd_select_channel(cD, channel);

    let scale_xy = select(
        vec2<f32>(0.5, 0.5),
        vec2<f32>(dims.x * 0.5, dims.y * 0.5),
        scale_mode == 1u,
    );
    let dx = diff_x * scale_xy.x;
    let dy = diff_y * scale_xy.y;

    return vec4<f32>(dx, dy, 0.0, 1.0);
}
