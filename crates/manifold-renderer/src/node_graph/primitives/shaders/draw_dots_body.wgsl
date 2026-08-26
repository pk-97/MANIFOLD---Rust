// `node.draw_dots` fusable body (D3, BUG-114). The `detections` port is
// tagged `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the
// codegen binds the storage global `buf_detections: array<Element>` (element
// struct synthesized from the port's Channels[X, Y, WIDTH, HEIGHT] signature)
// and this body references it directly by name — no pre-read, no body arg,
// exactly `BufferGather`'s ABI, just hosted in a texture-domain kernel.
//
// All metric math in PIXEL space (uv * dims): dots keep their shape at any
// aspect ratio. dpi_scale preserves the 1080p-reference look across
// resolutions.
fn body(
    c_in: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color: vec4<f32>,
    alpha: f32,
    radius_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let radius = radius_px * dpi_scale;
    let p = uv * dims;

    var coverage = 0.0;
    let n = arrayLength(&buf_detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = buf_detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let center = (vec2<f32>(d.x, d.y) + vec2<f32>(d.width, d.height) * 0.5) * dims;
        let dist = length(p - center);
        coverage = max(coverage, 1.0 - saturate(dist / radius));
    }

    let add = coverage * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
