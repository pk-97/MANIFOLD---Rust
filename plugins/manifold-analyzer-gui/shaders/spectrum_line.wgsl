// Spectrum + spectrogram — fullscreen fragment shader.
//
// Top region (y < spectrum_height):
//   SPAN-style Mid + Side line/fill curves. Pipeline:
//     1. For each curve, compute dB at the pixel centre frequency and at
//        ±1 px in log-freq (for anti-aliased line SDF).
//     2. Apply 1/N-oct frequency smoothing in the power domain.
//     3. Apply +slope dB/oct tilt around a reference frequency and a
//        scalar align-0-dB offset.
//     4. Convert dB → y pixel, evaluate SDF to adjacent pixel anchors, AA.
//     5. Compose: bg → side fill → side line → mid fill → mid line.
//   Side is drawn underneath Mid so the primary curve reads as foreground.
//
// Bottom region (y >= spectrum_height):
//   Scrolling Mid spectrogram sampled from a ring buffer. One row of pixels
//   = one historical column. Newest column is at the top (just below the
//   spectrum line), flowing down with age. dB → colour via inferno-like
//   ramp, with the same display tilt applied so the ramp has headroom at
//   low frequencies.

struct Uniforms {
    resolution: vec2<f32>,
    sample_rate: f32,
    fft_size: f32,
    freq_min: f32,
    freq_max: f32,
    db_min: f32,
    db_max: f32,
    line_color: vec4<f32>,
    bg_color: vec4<f32>,
    side_color: vec4<f32>,
    line_thickness: f32,
    slope_db_per_oct: f32,
    slope_ref_freq: f32,
    align_offset_db: f32,
    smooth_half_oct_log2: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> mid_spectrum: array<f32>;
@group(0) @binding(2) var<storage, read> side_spectrum: array<f32>;
@group(0) @binding(3) var<storage, read> history: array<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let pos = p[vi];
    var out: VsOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.uv = vec2<f32>((pos.x + 1.0) * 0.5, 1.0 - (pos.y + 1.0) * 0.5);
    return out;
}

fn db_to_y_px(db: f32) -> f32 {
    let t = (db - u.db_min) / (u.db_max - u.db_min);
    return u.spectrum_height * (1.0 - clamp(t, 0.0, 1.0));
}

fn tilt_db(freq: f32, raw_db: f32) -> f32 {
    return raw_db
         + u.slope_db_per_oct * log2(freq / u.slope_ref_freq)
         + u.align_offset_db;
}

// Number of samples taken inside the smoothing bandwidth. Fixed loop for
// simple, branchless sampling; 12 gives a clean 1/12-oct rectangular window.
const SMOOTH_N: i32 = 12;

fn sample_bin_db_mid(bin_f: f32, num_bins: f32) -> f32 {
    if (bin_f < 0.0 || bin_f > num_bins - 1.0) {
        return u.db_min;
    }
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, u32(num_bins) - 1u);
    let frac = fract(bin_f);
    return mix(mid_spectrum[bin_lo], mid_spectrum[bin_hi], frac);
}

fn sample_bin_db_side(bin_f: f32, num_bins: f32) -> f32 {
    if (bin_f < 0.0 || bin_f > num_bins - 1.0) {
        return u.db_min;
    }
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, u32(num_bins) - 1u);
    let frac = fract(bin_f);
    return mix(side_spectrum[bin_lo], side_spectrum[bin_hi], frac);
}

fn smoothed_db_mid(freq: f32) -> f32 {
    let num_bins = u.fft_size * 0.5;
    let bins_per_hz = u.fft_size / u.sample_rate;
    if (u.smooth_half_oct_log2 <= 0.0) {
        return sample_bin_db_mid(freq * bins_per_hz, num_bins);
    }
    let log_c = log(freq);
    let log_half = u.smooth_half_oct_log2 * 0.6931471805599453; // ln(2)
    let log_lo = log_c - log_half;
    let log_hi = log_c + log_half;
    var power_sum = 0.0;
    for (var i: i32 = 0; i < SMOOTH_N; i = i + 1) {
        let t = (f32(i) + 0.5) / f32(SMOOTH_N);
        let f = exp(mix(log_lo, log_hi, t));
        let db = sample_bin_db_mid(f * bins_per_hz, num_bins);
        power_sum = power_sum + pow(10.0, db * 0.1);
    }
    let avg_power = power_sum / f32(SMOOTH_N);
    return 10.0 * log(avg_power + 1e-24) / log(10.0);
}

fn smoothed_db_side(freq: f32) -> f32 {
    let num_bins = u.fft_size * 0.5;
    let bins_per_hz = u.fft_size / u.sample_rate;
    if (u.smooth_half_oct_log2 <= 0.0) {
        return sample_bin_db_side(freq * bins_per_hz, num_bins);
    }
    let log_c = log(freq);
    let log_half = u.smooth_half_oct_log2 * 0.6931471805599453;
    let log_lo = log_c - log_half;
    let log_hi = log_c + log_half;
    var power_sum = 0.0;
    for (var i: i32 = 0; i < SMOOTH_N; i = i + 1) {
        let t = (f32(i) + 0.5) / f32(SMOOTH_N);
        let f = exp(mix(log_lo, log_hi, t));
        let db = sample_bin_db_side(f * bins_per_hz, num_bins);
        power_sum = power_sum + pow(10.0, db * 0.1);
    }
    let avg_power = power_sum / f32(SMOOTH_N);
    return 10.0 * log(avg_power + 1e-24) / log(10.0);
}

fn y_px_at_freq_mid(freq: f32) -> f32 {
    return db_to_y_px(tilt_db(freq, smoothed_db_mid(freq)));
}

fn y_px_at_freq_side(freq: f32) -> f32 {
    return db_to_y_px(tilt_db(freq, smoothed_db_side(freq)));
}

fn sdf_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

// Vision 4X "Heatmap" style — jet-like ramp from black through deep blue,
// cyan, green, yellow, orange to saturated red. Low dB = black/navy so
// near-silent regions read as background; mids = cyan/green; peaks = red.
fn colormap(t_in: f32) -> vec3<f32> {
    let t = clamp(t_in, 0.0, 1.0);
    let c0 = vec3<f32>(0.00, 0.00, 0.00); // black
    let c1 = vec3<f32>(0.00, 0.00, 0.45); // deep navy
    let c2 = vec3<f32>(0.00, 0.10, 0.95); // blue
    let c3 = vec3<f32>(0.00, 0.80, 0.95); // cyan
    let c4 = vec3<f32>(0.20, 0.95, 0.20); // green
    let c5 = vec3<f32>(0.95, 0.95, 0.00); // yellow
    let c6 = vec3<f32>(0.95, 0.00, 0.00); // red
    if (t < 0.15) { return mix(c0, c1, t / 0.15); }
    if (t < 0.35) { return mix(c1, c2, (t - 0.15) / 0.20); }
    if (t < 0.55) { return mix(c2, c3, (t - 0.35) / 0.20); }
    if (t < 0.70) { return mix(c3, c4, (t - 0.55) / 0.15); }
    if (t < 0.85) { return mix(c4, c5, (t - 0.70) / 0.15); }
    return mix(c5, c6, (t - 0.85) / 0.15);
}

// History is pre-resampled to `log_bins` log-spaced dB values per column
// by the CPU side — at low freq each log bin is finer than an FFT bin
// (upsampled via power-domain linear interp), at high freq each log bin
// integrates power across many FFT bins (anti-aliased). Indexing is
// simple: `log_bin=0` is `freq_min` (bottom), `log_bin=log_bins-1` is
// `freq_max` (top). Pixel-side interpolation is one 2-tap linear blend.
fn sample_history_db(col: i32, log_bin_f: f32, log_bins_i: i32) -> f32 {
    let clamped = clamp(log_bin_f, 0.0, f32(log_bins_i) - 1.0);
    let lo = i32(floor(clamped));
    let hi = min(lo + 1, log_bins_i - 1);
    let frac = fract(clamped);
    let db_lo = history[col * log_bins_i + lo];
    let db_hi = history[col * log_bins_i + hi];
    let p_lo = pow(10.0, db_lo * 0.1);
    let p_hi = pow(10.0, db_hi * 0.1);
    let p = mix(p_lo, p_hi, frac);
    return 10.0 * log(p + 1e-24) / log(10.0);
}

fn spectrogram_pixel(px: vec2<f32>) -> vec4<f32> {
    let history_cols_i = i32(u.history_cols);
    let write_col_i = i32(u.write_col);
    let log_bins_i = i32(u.log_bins);

    // X axis: newest column at the right edge, older frames scrolling to
    // the left (standard DAW/Vision 4X convention). `history_idx` counts
    // pixels back from the right edge.
    let history_idx = i32(floor(u.resolution.x - 1.0 - px.x));
    if (history_idx < 0 || history_idx >= history_cols_i) {
        return vec4<f32>(0.0);
    }
    var col = write_col_i - history_idx;
    col = ((col % history_cols_i) + history_cols_i) % history_cols_i;

    // Y axis: log-spaced bins, high freq at the top. rel_y=0 → top = last
    // log bin (freq_max); rel_y=1 → bottom = first log bin (freq_min).
    let spec_y = px.y - u.spectrum_height;
    let spec_h = max(u.resolution.y - u.spectrum_height, 1.0);
    let rel_y = clamp(spec_y / spec_h, 0.0, 1.0);
    let log_bin_f = (1.0 - rel_y) * (u.log_bins - 1.0);

    let raw_db = sample_history_db(col, log_bin_f, log_bins_i);

    // No tilt — Vision 4X's heatmap is raw dB. Colourmap range is
    // independent of the spectrum curve's axis range.
    let span = max(u.spectrogram_db_max - u.spectrogram_db_min, 1e-3);
    let t = (raw_db - u.spectrogram_db_min) / span;
    let rgb = colormap(t);
    return vec4<f32>(rgb, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.uv * u.resolution;

    let log_lo = log(u.freq_min);
    let log_hi = log(u.freq_max);
    let log_range = log_hi - log_lo;
    let dx_per_px = log_range / u.resolution.x;
    let log_f = log_lo + in.uv.x * log_range;
    let freq = exp(log_f);

    // Spectrogram region — opaque fill, no curves. Uses its own freq axis
    // (log-y) so `freq` from the curve-mapping above is not used here.
    if (px.y >= u.spectrum_height) {
        return spectrogram_pixel(px);
    }

    // Spectrum region — curves + fills composited over transparency so the
    // egui-drawn grid behind the paint callback shows through.
    let my_prev = y_px_at_freq_mid(exp(log_f - dx_per_px));
    let my_curr = y_px_at_freq_mid(exp(log_f));
    let my_next = y_px_at_freq_mid(exp(log_f + dx_per_px));
    let ma = vec2<f32>(px.x - 1.0, my_prev);
    let mb = vec2<f32>(px.x,       my_curr);
    let mc = vec2<f32>(px.x + 1.0, my_next);
    let dm = min(sdf_segment(px, ma, mb), sdf_segment(px, mb, mc));

    let sy_prev = y_px_at_freq_side(exp(log_f - dx_per_px));
    let sy_curr = y_px_at_freq_side(exp(log_f));
    let sy_next = y_px_at_freq_side(exp(log_f + dx_per_px));
    let sa = vec2<f32>(px.x - 1.0, sy_prev);
    let sb = vec2<f32>(px.x,       sy_curr);
    let sc = vec2<f32>(px.x + 1.0, sy_next);
    let ds = min(sdf_segment(px, sa, sb), sdf_segment(px, sb, sc));

    let half_t = u.line_thickness * 0.5;
    let aa_mid  = 1.0 - smoothstep(half_t - 0.5, half_t + 0.5, dm);
    let aa_side = 1.0 - smoothstep(half_t - 0.5, half_t + 0.5, ds);

    // Fill coverage: 1 below the curve, 0 above, with a 1-px AA edge. The
    // lower bound is spectrum_height — fills stop at the spectrogram seam.
    let lower = min(px.y, u.spectrum_height);
    let fill_mid  = smoothstep(my_curr - 0.5, my_curr + 0.5, lower);
    let fill_side = smoothstep(sy_curr - 0.5, sy_curr + 0.5, lower);

    let a_side_fill = fill_side * u.fill_alpha;
    let a_side_line = aa_side;
    let a_mid_fill  = fill_mid  * u.fill_alpha;
    let a_mid_line  = aa_mid;

    // Premultiplied-alpha "over" compositing, far-to-near (side fill below,
    // mid line on top).
    var pre = vec3<f32>(0.0);
    var alpha = 0.0;

    pre = u.side_color.rgb * a_side_fill + pre * (1.0 - a_side_fill);
    alpha = a_side_fill + alpha * (1.0 - a_side_fill);

    pre = u.side_color.rgb * a_side_line + pre * (1.0 - a_side_line);
    alpha = a_side_line + alpha * (1.0 - a_side_line);

    pre = u.line_color.rgb * a_mid_fill + pre * (1.0 - a_mid_fill);
    alpha = a_mid_fill + alpha * (1.0 - a_mid_fill);

    pre = u.line_color.rgb * a_mid_line + pre * (1.0 - a_mid_line);
    alpha = a_mid_line + alpha * (1.0 - a_mid_line);

    return vec4<f32>(pre, alpha);
}
