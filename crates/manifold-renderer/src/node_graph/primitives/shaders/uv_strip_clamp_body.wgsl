// node.uv_strip_clamp — fusable body (freeze §12), SOURCE. Edge-stretch coordinate
// generator: clamp the per-pixel UV to a centre strip of width `width` on the
// selected axis (0 Horiz / 1 Vert / 2 Both); pixels outside collapse to the edge.
// Output (clamped_u, clamped_v, 0, 1). Matches uv_strip_clamp.wgsl. PARAMS:
// [width, mode (Enum->u32)].
fn body(uv: vec2<f32>, dims: vec2<f32>, width: f32, mode: u32) -> vec4<f32> {
    let half_width = width * 0.5;
    let lo = 0.5 - half_width;
    let hi = 0.5 + half_width;

    var s = uv;
    if mode == 0u || mode == 2u {
        s.x = clamp(uv.x, lo, hi);
    }
    if mode == 1u || mode == 2u {
        s.y = clamp(uv.y, lo, hi);
    }
    return vec4<f32>(s.x, s.y, 0.0, 1.0);
}
