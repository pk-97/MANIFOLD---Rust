// Spectrogram sweep-fill: sample a ring buffer of VQT magnitude columns and
// paint a stationary, colour-mapped time-frequency image with a write head that
// sweeps left→right (oscilloscope style).
//
// Layout: `history` is `num_cols` columns of `num_bins` magnitudes each, column
// c at `[c*num_bins .. (c+1)*num_bins)`. `num_cols` equals the on-screen pixel
// width, so x maps 1:1 to a column slot — no resampling. `write_index` is the
// slot the NEXT column will overwrite (the sweep line): slots to its LEFT were
// written this pass (more recent), slots to its right are from the previous
// pass (older). y maps to log-frequency (VQT bins are geometrically spaced, so
// bin index is linear in y). Magnitudes are `|VQT|` (unit sine ≈ 1.0); we map
// dB → a jet colour ramp (matched to the Analyzer VST).

struct Params {
    num_bins: u32,
    num_cols: u32,
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
    // Pink-tilt slope (dB/octave) and the displayed range's octave span
    // `log2(fmax/fmin)`. The colourmap input is tilted by
    // `slope * log2(f / geomean)` so pink noise reads flat; centred on the
    // geometric-mean frequency keeps average brightness constant. Slope 0
    // disables the tilt (Flat).
    tilt_slope: f32,
    freq_log_ratio: f32,
    // Cursor frequency line (uv.y, 0 top → 1 bottom); negative hides it.
    cursor_y: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<storage, read> history: array<f32>;
@group(0) @binding(1) var<uniform> p: Params;
// Per-column overlay scalars, 2 per column: [centroid_yfb, onset]. Same column
// layout as `history`. `centroid_yfb` is the spectral centroid as height-from-
// bottom (0..1); < 0 hides it. `onset` is a 0..1 transient impulse.
@group(0) @binding(2) var<storage, read> col_scalars: array<f32>;

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

// Heatmap-style jet ramp, matched to the Analyzer VST: black → navy → blue →
// cyan → green → yellow → red → white. The top 10% goes red→white so the
// loudest peaks separate clearly from merely-loud content (solves jet's
// classic red-vs-darker-red crush at the top end). Stops are non-uniform —
// kept verbatim from the VST's `spectrum_line.wgsl` so the two read identical.
fn colormap(t_in: f32) -> vec3<f32> {
    let t = clamp(t_in, 0.0, 1.0);
    let c0 = vec3<f32>(0.00, 0.00, 0.00); // black
    let c1 = vec3<f32>(0.00, 0.00, 0.45); // deep navy
    let c2 = vec3<f32>(0.00, 0.10, 0.95); // blue
    let c3 = vec3<f32>(0.00, 0.80, 0.95); // cyan
    let c4 = vec3<f32>(0.20, 0.95, 0.20); // green
    let c5 = vec3<f32>(0.95, 0.95, 0.00); // yellow
    let c6 = vec3<f32>(0.95, 0.00, 0.00); // red
    let c7 = vec3<f32>(1.00, 1.00, 1.00); // white — peaks
    if (t < 0.15) { return mix(c0, c1, t / 0.15); }
    if (t < 0.35) { return mix(c1, c2, (t - 0.15) / 0.20); }
    if (t < 0.55) { return mix(c2, c3, (t - 0.35) / 0.20); }
    if (t < 0.70) { return mix(c3, c4, (t - 0.55) / 0.15); }
    if (t < 0.80) { return mix(c4, c5, (t - 0.70) / 0.10); }
    if (t < 0.90) { return mix(c5, c6, (t - 0.80) / 0.10); }
    return mix(c6, c7, (t - 0.90) / 0.10);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n_bins = p.num_bins;
    let n_cols = p.num_cols;
    if (n_bins == 0u || n_cols == 0u) {
        return vec4<f32>(0.04, 0.04, 0.05, 1.0);
    }

    // x: direct 1:1 map to a column slot (no scrolling). Each pixel column owns
    // one ring column; the sweep head overwrites them in place.
    let col = min(u32(in.uv.x * f32(n_cols)), n_cols - 1u);

    // y: bottom (uv.y=1) → bin 0 (low freq); top (uv.y=0) → highest bin. Sample
    // with a 2-tap blend between adjacent log bins, interpolated in the POWER
    // domain (magnitudes are |VQT|, power = mag²) so the gradient is smooth
    // instead of blocky — matches the VST's `sample_history_db`.
    let log_bin_f = clamp((1.0 - in.uv.y) * f32(n_bins - 1u), 0.0, f32(n_bins - 1u));
    let lo = u32(floor(log_bin_f));
    let hi = min(lo + 1u, n_bins - 1u);
    let frac = log_bin_f - floor(log_bin_f);
    let mag_lo = history[col * n_bins + lo];
    let mag_hi = history[col * n_bins + hi];
    let power = mix(mag_lo * mag_lo, mag_hi * mag_hi, frac);
    let db = 10.0 * log2(power + 1e-18) * 0.30103; // 10·log10(power) = 20·log10(mag)

    // Pink tilt: boost highs / cut lows by `slope · log2(f/geomean)`. With the
    // bin axis linear in log-freq, log2(f/geomean) = freq_log_ratio·(0.5 - uv.y).
    let tilt = p.tilt_slope * p.freq_log_ratio * (0.5 - in.uv.y);
    let norm = clamp((db + tilt - p.db_min) / (p.db_max - p.db_min), 0.0, 1.0);
    var rgb = colormap(norm);

    // Sweep head: a thin bright vertical line at the next write slot, marking
    // "now" — the seam between this pass (left) and the previous one (right).
    let head_x = f32(p.write_index) / f32(n_cols);
    let head = vec3<f32>(0.95, 0.97, 1.0);
    if (abs(in.uv.x - head_x) < (1.0 / f32(n_cols))) {
        rgb = mix(rgb, head, 0.6);
    }

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

    // Per-column overlays: the spectral-centroid trace and transient ticks.
    // Both scroll with the waterfall because they're keyed by `col`, the same
    // 1:1 column slot the magnitudes use.
    let centroid_yfb = col_scalars[col * 2u];
    let onset = col_scalars[col * 2u + 1u];
    // Centroid trace: a magenta line tracking the column's centre of spectral
    // mass — "where the energy sits" over time. Soft-edged so it reads smooth.
    if (centroid_yfb >= 0.0) {
        let cd = abs(y_from_bottom - centroid_yfb);
        let w = clamp(1.0 - cd / 0.008, 0.0, 1.0);
        rgb = mix(rgb, vec3<f32>(1.0, 0.25, 0.85), w * 0.85);
    }
    // Transient ticks: a warm bar rising from the bottom edge on a column where
    // an onset fired, its height proportional to the impulse strength.
    if (onset > 0.04 && in.uv.y > 1.0 - onset * 0.14) {
        rgb = mix(rgb, vec3<f32>(1.0, 0.85, 0.4), clamp(onset, 0.0, 1.0));
    }

    // Cursor frequency locator: a faint horizontal line at the hovered freq,
    // paired with the title-row readout. `cursor_y` is in uv.y (0 top → 1 bottom).
    if (p.cursor_y >= 0.0 && abs(in.uv.y - p.cursor_y) < 0.0018) {
        rgb = mix(rgb, vec3<f32>(0.9, 0.95, 1.0), 0.45);
    }
    return vec4<f32>(rgb, 1.0);
}
