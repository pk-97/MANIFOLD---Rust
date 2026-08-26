// `node.draw_connections` fusable body (D3, BUG-114). Both the `detections`
// and `edges` ports are tagged `BufferIndex`
// (`input_access: [Coincident, BufferIndex, BufferIndex]`), so the codegen
// binds TWO storage globals — `buf_detections: array<Element>` (from
// Channels[X, Y, WIDTH, HEIGHT]) and `buf_edges: array<Element2>` (from
// Channels[A_INDEX, B_INDEX]) — the generic BufferIndex mechanism handles
// any number of tagged Array inputs on one atom, not just one (P4a proved
// it with a single array on draw_dots; this is the first atom to exercise
// two). Both referenced directly by name — no pre-read, no body arg.
//
// All metric math in PIXEL space (uv * dims): edges keep their dash shape
// and thickness at any aspect ratio. dpi_scale preserves the 1080p-reference
// look across resolutions.
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
    thickness_px: f32,
    dash_period_px: f32,
    dash_fill: f32,
    midpoint_radius_px: f32,
) -> vec4<f32> {
    let dpi_scale = dims.y / 1080.0;
    let thickness = thickness_px * dpi_scale;
    let dash_period = dash_period_px * dpi_scale;
    let mid_radius = midpoint_radius_px * dpi_scale;
    let p = uv * dims;
    let det_count = arrayLength(&buf_detections);

    var line_cov = 0.0;
    var mid_cov = 0.0;
    let n = arrayLength(&buf_edges);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let e = buf_edges[i];
        if e.a_index == 0xFFFFFFFFu { continue; }
        if e.a_index >= det_count || e.b_index >= det_count { continue; }
        let da = buf_detections[e.a_index];
        let db = buf_detections[e.b_index];
        let center_a = (vec2<f32>(da.x, da.y) + vec2<f32>(da.width, da.height) * 0.5) * dims;
        let center_b = (vec2<f32>(db.x, db.y) + vec2<f32>(db.width, db.height) * 0.5) * dims;

        let ba = center_b - center_a;
        let len_sq = dot(ba, ba);
        if len_sq < 0.000001 { continue; }
        let pa = p - center_a;
        let t_val = saturate(dot(pa, ba) / len_sq);
        let len = sqrt(len_sq);
        let dash_phase = fract(t_val * len / dash_period);
        let dash_mask = step(dash_fill, dash_phase);

        line_cov = max(line_cov, line_seg(p, center_a, center_b, thickness) * 0.5 * dash_mask);

        if mid_radius > 0.0 {
            let mid = (center_a + center_b) * 0.5;
            let mid_dist = length(p - mid);
            mid_cov = max(mid_cov, (1.0 - saturate(mid_dist / mid_radius)) * 0.4);
        }
    }

    let add = (line_cov + mid_cov) * alpha;
    return vec4<f32>(c_in.rgb + color.rgb * add, c_in.a);
}
