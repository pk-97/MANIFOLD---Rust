// Spectrogram waterfall: sample a ring buffer of VQT magnitude columns and
// paint a scrolling, colour-mapped time-frequency image.
//
// Layout: `history` is `history_len` columns of `num_bins` magnitudes each,
// column c at `[c*num_bins .. (c+1)*num_bins)`. It is a ring; `write_index` is
// where the NEXT column will be written, so the newest column is
// `write_index-1`. x maps to time (right = now), y to log-frequency (VQT bins
// are geometrically spaced, so bin index is linear in y). Magnitudes are
// `|VQT|` (unit sine ≈ 1.0); we map dB → a magma-style colour ramp.

struct Params {
    num_bins: u32,
    history_len: u32,
    write_index: u32,
    _pad0: u32,
    db_min: f32,
    db_max: f32,
    // Band-divider positions, normalised 0..1 from the bottom (low freq).
    // Negative = disabled. These are the low/mid and mid/high splits the
    // modulation reads, drawn as thin lines so the performer sees where energy
    // lands relative to the band a slider is driven from.
    band_lo_y: f32,
    band_hi_y: f32,
};

@group(0) @binding(0) var<storage, read> history: array<f32>;
@group(0) @binding(1) var<uniform> p: Params;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    // uv.x: 0 left → 1 right.  uv.y: 0 top → 1 bottom.
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// Magma-ish ramp: black → purple → magenta → orange → pale yellow.
fn colormap(t: f32) -> vec3<f32> {
    let c0 = vec3<f32>(0.001, 0.000, 0.014);
    let c1 = vec3<f32>(0.28, 0.09, 0.42);
    let c2 = vec3<f32>(0.63, 0.19, 0.39);
    let c3 = vec3<f32>(0.96, 0.49, 0.27);
    let c4 = vec3<f32>(0.99, 0.96, 0.78);
    let x = clamp(t, 0.0, 1.0) * 4.0;
    if (x < 1.0) { return mix(c0, c1, x); }
    if (x < 2.0) { return mix(c1, c2, x - 1.0); }
    if (x < 3.0) { return mix(c2, c3, x - 2.0); }
    return mix(c3, c4, x - 3.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n_bins = p.num_bins;
    let n_cols = p.history_len;
    if (n_bins == 0u || n_cols == 0u) {
        return vec4<f32>(0.04, 0.04, 0.05, 1.0);
    }

    // y: bottom (uv.y=1) → bin 0 (low freq); top (uv.y=0) → highest bin.
    let bin_f = (1.0 - in.uv.y) * f32(n_bins);
    let bin = min(u32(bin_f), n_bins - 1u);

    // x: right (uv.x=1) → newest. Walk back `age` columns from the newest.
    let age = u32((1.0 - in.uv.x) * f32(n_cols - 1u));
    // newest = write_index - 1 (mod n_cols); col = newest - age (mod n_cols).
    let col = (p.write_index + n_cols - 1u - age) % n_cols;

    let mag = history[col * n_bins + bin];
    let db = 20.0 * log2(mag + 1e-9) * 0.30103; // log10 = log2 * 0.30103
    let norm = clamp((db - p.db_min) / (p.db_max - p.db_min), 0.0, 1.0);
    var rgb = colormap(norm);

    // Band dividers: a thin bright line at each split, blended over the image.
    let y_from_bottom = 1.0 - in.uv.y;
    let line = vec3<f32>(0.85, 0.85, 0.90);
    let half_px = 0.0015;
    if (p.band_lo_y >= 0.0 && abs(y_from_bottom - p.band_lo_y) < half_px) {
        rgb = mix(rgb, line, 0.7);
    }
    if (p.band_hi_y >= 0.0 && abs(y_from_bottom - p.band_hi_y) < half_px) {
        rgb = mix(rgb, line, 0.7);
    }
    return vec4<f32>(rgb, 1.0);
}
