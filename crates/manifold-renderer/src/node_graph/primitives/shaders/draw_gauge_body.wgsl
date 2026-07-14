// `node.draw_gauge` fusable body (D3, BUG-114). The `detections` port is
// tagged `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the
// codegen binds the storage global `buf_detections: array<Element>` (element
// struct synthesized from the port's Channels[X, Y, WIDTH, HEIGHT] signature)
// and this body references it directly by name — no pre-read, no body arg,
// exactly `BufferGather`'s ABI, just hosted in a texture-domain kernel.
// Matches `draw_gauge.wgsl`'s per-detection gauge math verbatim.
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
    bottom_offset_px: f32,
    bar_height_px: f32,
    min_bar_width_px: f32,
    fill_scale: f32,
    thickness_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let px_u = (1.0 / dims.x) * dpi_scale;
    let px_v = (1.0 / dims.y) * dpi_scale;
    let thickness = thickness_px * px_u;
    let bar_height = bar_height_px * px_v;
    let bottom_offset = bottom_offset_px * px_v;
    let min_bar_w = min_bar_width_px * px_u;

    var coverage = 0.0;
    let n = arrayLength(&buf_detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = buf_detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);

        let origin = vec2<f32>(center.x - half_size.x, center.y + half_size.y + bottom_offset);
        let bar_w = max(d.width, min_bar_w);
        let fill_frac = saturate(d.width * d.height * fill_scale);

        let tl = origin;
        let tr = origin + vec2<f32>(bar_w, 0.0);
        let bl = origin + vec2<f32>(0.0, bar_height);
        let br = origin + vec2<f32>(bar_w, bar_height);
        coverage = max(coverage, line_seg(uv, tl, tr, thickness));
        coverage = max(coverage, line_seg(uv, bl, br, thickness));
        coverage = max(coverage, line_seg(uv, tl, bl, thickness));
        coverage = max(coverage, line_seg(uv, tr, br, thickness));

        let rel = uv - origin;
        if rel.x >= 0.0 && rel.x <= bar_w * fill_frac && rel.y >= 0.0 && rel.y <= bar_height {
            coverage = max(coverage, 0.4);
        }
    }

    let add = coverage * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
