// `node.draw_markers` fusable body. The `detections` port is tagged
// `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the codegen
// binds the storage global `buf_detections: array<Element>` and this body
// references it directly by name — no pre-read, no body arg.
//
// All metric math in PIXEL space (uv * dims): markers keep their shape at
// any aspect ratio. dpi_scale preserves the 1080p-reference look across
// resolutions.
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
    symbol: u32,
    color: vec4<f32>,
    alpha: f32,
    size_fraction: f32,
    thickness_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let thickness = thickness_px * dpi_scale;
    let p = uv * dims;

    var coverage = 0.0;
    let n = arrayLength(&buf_detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = buf_detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5 * dims;
        let center = (vec2<f32>(d.x, d.y) + vec2<f32>(d.width, d.height) * 0.5) * dims;
        let arm = min(half_size.x, half_size.y) * size_fraction;

        if symbol == 0u {
            let tl = center - half_size;
            let tr = vec2<f32>(center.x + half_size.x, center.y - half_size.y);
            let bl = vec2<f32>(center.x - half_size.x, center.y + half_size.y);
            let br = center + half_size;

            coverage = max(coverage, line_seg(p, tl, tl + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(p, tl, tl + vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(p, tr, tr - vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(p, tr, tr + vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(p, bl, bl + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(p, bl, bl - vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(p, br, br - vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(p, br, br - vec2<f32>(0.0, arm), thickness));
        } else {
            coverage = max(coverage, line_seg(p, center - vec2<f32>(arm, 0.0), center + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(p, center - vec2<f32>(0.0, arm), center + vec2<f32>(0.0, arm), thickness));
        }
    }

    let add = coverage * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
