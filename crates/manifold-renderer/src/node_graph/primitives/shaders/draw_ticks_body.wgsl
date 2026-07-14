// `node.draw_ticks` fusable body (D3, BUG-114). The `detections` port is
// tagged `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the
// codegen binds the storage global `buf_detections: array<Element>` (element
// struct synthesized from the port's Channels[X, Y, WIDTH, HEIGHT] signature)
// and this body references it directly by name — no pre-read, no body arg,
// exactly `BufferGather`'s ABI, just hosted in a texture-domain kernel.
// Matches `draw_ticks.wgsl`'s per-detection tick math verbatim.
fn line_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, thickness: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let len_sq = dot(ba, ba);
    if len_sq < 0.000001 { return 0.0; }
    let h = saturate(dot(pa, ba) / len_sq);
    let d = length(pa - ba * h);
    return 1.0 - saturate(d / thickness);
}

fn body(
    c_in: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color: vec4<f32>,
    alpha: f32,
    right_offset_px: f32,
    long_tick_px: f32,
    short_tick_px: f32,
    thickness_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let px_u = (1.0 / dims.x) * dpi_scale;
    let thickness = thickness_px * px_u;
    let right_offset = right_offset_px * px_u;

    var coverage = 0.0;
    let n = arrayLength(&buf_detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = buf_detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);

        let tick_base = vec2<f32>(center.x + half_size.x + right_offset, center.y - half_size.y);
        let tick_spacing = half_size.y * 0.5;

        for (var t: u32 = 0u; t < 4u; t = t + 1u) {
            let tick_start = tick_base + vec2<f32>(0.0, tick_spacing * f32(t));
            let tick_len = select(short_tick_px * px_u, long_tick_px * px_u, (t % 2u) == 0u);
            coverage = max(coverage, line_seg(uv, tick_start, tick_start + vec2<f32>(tick_len, 0.0), thickness) * 0.5);
        }
    }

    let add = coverage * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
