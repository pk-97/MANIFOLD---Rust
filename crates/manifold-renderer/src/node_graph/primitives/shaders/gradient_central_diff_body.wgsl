// node.gradient_central_diff — fusable body (freeze §12), GATHER_TEXEL. Per-pixel
// central-difference gradient of one channel of `in`: reads the 4 axis
// neighbours via an EXACT integer textureLoad (one texel index step), with
// the boundary policy resolved MANUALLY from `wrap_mode` (see
// gcd_wrap_coord) instead of a sampler's address mode. D6(a)
// (docs/DEPTH_RELIGHT_DESIGN.md): converted from a filtering-sampler Gather
// read so `in` can carry `precision_critical` — every offset lands on an
// exact texel center (uv = (id+0.5)/dims, offset by exactly ±1 texel), so
// textureLoad+clamp/modulo agrees with the old textureSampleLevel+sampler
// bit-for-bit (proven by
// gpu_tests::gather_texel_conversion_is_value_preserving_{clamp,repeat}).
// scale_mode 0 = Texel (×0.5), 1 = UV (×dim*0.5 per axis). Matches
// gradient_central_diff.wgsl. PARAMS: [channel (Enum->u32), scale_mode
// (Enum->u32), wrap_mode (Enum->u32)].
fn gcd_select_channel(c: vec4<f32>, idx: u32) -> f32 {
    switch idx {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        default: { return c.a; }
    }
}

// Resolve a neighbour texel index per `wrap_mode`: 0 (Clamp) clamps to the
// texture bounds — the ClampToEdge-sampler equivalent, exact because the
// offset is always precisely one texel from a fragment sampled at its own
// texel center. 1 (Repeat) modulo-wraps the index — the Repeat-address-
// sampler equivalent, exact for the same reason (a periodic sampler wrap at
// an exact one-texel-out-of-range UV lands on the exact opposite-edge texel
// center, identical to wrapping the integer index).
fn gcd_wrap_coord(c: vec2<i32>, dims_i: vec2<i32>, wrap_mode: u32) -> vec2<i32> {
    if wrap_mode == 1u {
        return ((c % dims_i) + dims_i) % dims_i;
    }
    return clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
}

fn body(in_tex: texture_2d<f32>, uv: vec2<f32>, dims: vec2<f32>, channel: u32, scale_mode: u32, wrap_mode: u32) -> vec4<f32> {
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);

    let cL = textureLoad(in_tex, gcd_wrap_coord(c - vec2<i32>(1, 0), dims_i, wrap_mode), 0);
    let cR = textureLoad(in_tex, gcd_wrap_coord(c + vec2<i32>(1, 0), dims_i, wrap_mode), 0);
    let cD = textureLoad(in_tex, gcd_wrap_coord(c - vec2<i32>(0, 1), dims_i, wrap_mode), 0);
    let cU = textureLoad(in_tex, gcd_wrap_coord(c + vec2<i32>(0, 1), dims_i, wrap_mode), 0);

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
