// `node.draw_dots` fusable body (D3, BUG-114). The `detections` port is
// tagged `BufferIndex` (`input_access: [Coincident, BufferIndex]`), so the
// codegen binds the storage global `buf_detections: array<Element>` (element
// struct synthesized from the port's Channels[X, Y, WIDTH, HEIGHT] signature)
// and this body references it directly by name — no pre-read, no body arg,
// exactly `BufferGather`'s ABI, just hosted in a texture-domain kernel.
// Matches `draw_dots.wgsl`'s per-detection coverage math verbatim.
fn body(
    c_in: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    color: vec4<f32>,
    alpha: f32,
    radius_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let radius = radius_px * (1.0 / dims.x) * dpi_scale;

    var coverage = 0.0;
    let n = arrayLength(&buf_detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = buf_detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let center = vec2<f32>(d.x + d.width * 0.5, d.y + d.height * 0.5);
        let dist = length(uv - center);
        coverage = max(coverage, 1.0 - saturate(dist / radius));
    }

    let add = coverage * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
