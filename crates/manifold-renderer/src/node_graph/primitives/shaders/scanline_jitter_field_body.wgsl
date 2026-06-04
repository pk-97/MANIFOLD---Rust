// node.scanline_jitter_field — fusable body (freeze §12), SOURCE. Per-row random
// horizontal-offset field (VHS/horizontal-tearing). Hashes each scanline row
// (animated by `time`), emits R = signed horizontal UV shift gated by `scanline`.
// res from the ambient dims; sjf_hash1 uses GPU sin (matches hand). `time` is a
// backing param (port-shadow; run() packs the resolved value). Matches
// scanline_jitter_field.wgsl. PARAMS: [amount, scanline, speed, time].
fn sjf_hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

fn body(uv: vec2<f32>, dims: vec2<f32>, amount: f32, scanline: f32, speed: f32, time: f32) -> vec4<f32> {
    let res = dims;
    let t = floor(time * speed * 12.0);

    let scanline_row = floor(uv.y * res.y);
    let scan_hash = sjf_hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - scanline * amount * 0.3, scan_hash);
    let scan_shift = (sjf_hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * amount * 0.08;
    let offset_x = scan_shift * scan_mask;

    return vec4<f32>(offset_x, 0.0, 0.0, 1.0);
}
