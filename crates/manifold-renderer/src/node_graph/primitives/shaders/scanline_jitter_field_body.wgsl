// node.scanline_jitter_field — fusable body (freeze §12), SOURCE. Per-row
// horizontal-offset field. motion=0 Tear (VHS jolt, byte-identical to the
// original sine-hash + scanline gate); motion=1 Slide (smooth value-noise
// per-band drift, the Latent Space website mosh slide). res from the ambient
// dims; sjf_hash1 uses GPU sin (matches hand). `time` is a backing param
// (port-shadow; run() packs the resolved value). Matches
// scanline_jitter_field.wgsl. PARAMS: [amount, scanline, speed, motion, bands, time].
fn sjf_hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

// Value-noise field (123.34/456.21/45.32 hash) — smooth, in [0,1]. Same field
// node.noise emits as its Value type; lifted here so Slide is self-contained.
fn sjf_value_hash(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p = p + dot(p, p + 45.32);
    return fract(p.x * p.y);
}
fn sjf_value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(sjf_value_hash(i),                       sjf_value_hash(i + vec2<f32>(1.0, 0.0)), uu.x),
        mix(sjf_value_hash(i + vec2<f32>(0.0, 1.0)), sjf_value_hash(i + vec2<f32>(1.0, 1.0)), uu.x),
        uu.y
    );
}

fn body(uv: vec2<f32>, dims: vec2<f32>, amount: f32, scanline: f32, speed: f32, motion: u32, bands: f32, time: f32) -> vec4<f32> {
    let res = dims;

    if motion == 1u {
        // Slide — smooth, ungated, every band drifts. speed=2 → website 0.13.
        // bands = 0 → no rows (offset 0): a downstream flow/domain warp carries
        // the motion instead of slicing the image into per-row tears.
        if bands <= 0.0 {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        let band = floor(uv.y * bands);
        let t = time * speed * 0.065;
        let n = sjf_value_noise(vec2<f32>(band, t));
        let offset_x = (n - 0.5) * amount * 0.05;
        return vec4<f32>(offset_x, 0.0, 0.0, 1.0);
    }

    // Tear (default) — byte-identical to the original VHS jolt.
    let t = floor(time * speed * 12.0);
    let scanline_row = floor(uv.y * res.y);
    let scan_hash = sjf_hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - scanline * amount * 0.3, scan_hash);
    let scan_shift = (sjf_hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * amount * 0.08;
    let offset_x = scan_shift * scan_mask;

    return vec4<f32>(offset_x, 0.0, 0.0, 1.0);
}
