// node.downsample — fusable body (freeze §12), GATHER. Integer-factor box-filter
// downsample. Reads factor×factor source texels per output texel (the effective
// factor is derived from in_dims/out_dims, NOT the uniform `factor` which is kept
// for diagnostics) and writes their mean via textureLoad (exact integer reads — a
// box filter wants no bilinear blend, so the bound sampler goes unused, matching
// the hand shader). The output pixel id is recovered from uv (= (id+0.5)/dims, so
// uv*dims truncates back to id). Matches downsample.wgsl. PARAMS: [factor (Enum,
// diagnostic — the body ignores it and uses the dim ratio)].
fn body(input_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, factor: u32) -> vec4<f32> {
    let out_dims = vec2<u32>(dims);
    let out_px = vec2<u32>(uv * dims); // = id (uv*dims = id+0.5, truncates to id)
    let in_dims = textureDimensions(input_tex);
    // Effective factor = input / output, clamped to >=1.
    let fx = max(1u, in_dims.x / out_dims.x);
    let fy = max(1u, in_dims.y / out_dims.y);
    let base = vec2<i32>(out_px * vec2<u32>(fx, fy));

    var sum = vec4<f32>(0.0);
    var taps: u32 = 0u;
    for (var dy: u32 = 0u; dy < fy; dy = dy + 1u) {
        for (var dx: u32 = 0u; dx < fx; dx = dx + 1u) {
            let coord = base + vec2<i32>(i32(dx), i32(dy));
            // Exclude taps past the right/bottom edge so the mean stays correct
            // when out_dims doesn't divide in_dims evenly.
            if (coord.x < i32(in_dims.x) && coord.y < i32(in_dims.y)) {
                sum = sum + textureLoad(input_tex, coord, 0);
                taps = taps + 1u;
            }
        }
    }

    let inv = 1.0 / f32(max(taps, 1u));
    return sum * inv;
}
