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
    smooth_half_oct_log2: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
    sync_mode: f32,
    cqt_fmin_hz: f32,
    cqt_bins_per_octave: f32,
    spectrogram_gamma: f32,
    // 0 = fixed-bandwidth smoothing via `smooth_half_oct_log2`.
    // 1 = ERB (Moore & Glasberg) — per-pixel half-width derived from
    //     the critical-band curve, much wider at the low end and
    //     tightening toward ~1/9 oct above 5 kHz.
    smoothing_mode: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> mid_spectrum: array<f32>;
@group(0) @binding(2) var<storage, read> side_spectrum: array<f32>;
@group(0) @binding(3) var<storage, read> history: array<f32>;
// Pre-computed weighting curve (dB) over log-uniform [freq_min, freq_max].
// CPU-side: GUI builds this whenever the weighting mode or freq range
// changes. Includes the DC-bias align offset baked in. Replaces the
// per-pixel biquad sin/cos evaluation the shader used to do.
@group(0) @binding(4) var<storage, read> weighting_lut: array<f32>;

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

// Sample the pre-computed weighting LUT by log-freq index. The LUT covers
// [log(freq_min), log(freq_max)] uniformly and has `align_offset_db`
// baked in, so `tilt_db` is just `raw + lut(freq)`.
fn weighting_db(freq: f32) -> f32 {
    let n = f32(arrayLength(&weighting_lut));
    if (n < 2.0) {
        return 0.0;
    }
    let t = clamp(log(freq / u.freq_min) / log(u.freq_max / u.freq_min), 0.0, 1.0);
    let idx_f = t * (n - 1.0);
    let lo = u32(floor(idx_f));
    let hi = min(lo + 1u, u32(n) - 1u);
    let frac = fract(idx_f);
    return mix(weighting_lut[lo], weighting_lut[hi], frac);
}

fn tilt_db(freq: f32, raw_db: f32) -> f32 {
    return raw_db + weighting_db(freq);
}

// Upper bound on smoothing taps per pixel. The smoothing loop strides
// across FFT bins inside the window: narrow windows hit every bin
// (few taps, cheap), wide windows stride past enough bins to stay
// under this cap (bounded cost). 64 handles 1/3-oct at 10 kHz with a
// 16k FFT (~860 bins in window → stride ~13). Bigger = more accurate
// but slower per pixel.
const SMOOTH_N_MAX: i32 = 64;

fn sample_bin_db_mid(bin_f: f32, num_bins: f32) -> f32 {
    if (bin_f < 0.0 || bin_f >= num_bins) {
        return u.db_min;
    }
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, u32(num_bins) - 1u);
    let frac = fract(bin_f);
    return mix(mid_spectrum[bin_lo], mid_spectrum[bin_hi], frac);
}

fn sample_bin_db_side(bin_f: f32, num_bins: f32) -> f32 {
    if (bin_f < 0.0 || bin_f >= num_bins) {
        return u.db_min;
    }
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, u32(num_bins) - 1u);
    let frac = fract(bin_f);
    return mix(side_spectrum[bin_lo], side_spectrum[bin_hi], frac);
}

// Per-pixel smoothing half-width in log2-octaves. Fixed-mode returns
// the uniform; ERB mode computes Moore & Glasberg's critical-band half-
// width at `freq`. Equivalent rectangular bandwidth:
//   ERB_hz(f) = 24.7 * (4.37 * f / 1000 + 1)
// Half-width in octaves = log2((f + erb/2) / f) = log2(1 + erb/(2f)).
fn half_octaves_at(freq: f32) -> f32 {
    if (u.smoothing_mode > 0.5) {
        if (freq <= 1e-3) {
            return 0.0;
        }
        let erb = 24.7 * (4.37 * freq * 1e-3 + 1.0);
        return log2(1.0 + (erb * 0.5) / freq);
    }
    return u.smooth_half_oct_log2;
}

fn smoothed_db_mid(freq: f32) -> f32 {
    let num_bins = u.fft_size * 0.5;
    let bins_per_hz = u.fft_size / u.sample_rate;
    let half_oct = half_octaves_at(freq);
    if (half_oct <= 0.0) {
        return sample_bin_db_mid(freq * bins_per_hz, num_bins);
    }
    // Stride the smoothing window in bin-index space. When the window
    // covers fewer bins than the tap cap we hit every bin (step=1); for
    // wide windows we stride across so the loop count stays bounded and
    // adjacent pixels sample a near-identical bin set (no aliasing as
    // centre freq shifts pixel to pixel).
    let bin_lo_f = clamp(freq * exp2(-half_oct) * bins_per_hz, 0.0, num_bins - 1.0);
    let bin_hi_f = clamp(freq * exp2(half_oct) * bins_per_hz, 0.0, num_bins - 1.0);
    let span_bins = max(bin_hi_f - bin_lo_f, 1e-6);
    // Hold tap count constant so adjacent pixels don't swap sample
    // counts as the window slides (an integer-n refactor produced
    // visible sharp dips where the ceil boundary crossed). Fractional
    // step handles narrow windows by over-sampling inside one bin,
    // which costs nothing (sample_bin_db_mid is just a linear interp).
    let step = span_bins / f32(SMOOTH_N_MAX);
    var power_sum = 0.0;
    for (var i: i32 = 0; i < SMOOTH_N_MAX; i = i + 1) {
        let b_f = bin_lo_f + (f32(i) + 0.5) * step;
        let db = sample_bin_db_mid(b_f, num_bins);
        power_sum = power_sum + pow(10.0, db * 0.1);
    }
    return 10.0 * log(power_sum / f32(SMOOTH_N_MAX) + 1e-24) / log(10.0);
}

fn smoothed_db_side(freq: f32) -> f32 {
    let num_bins = u.fft_size * 0.5;
    let bins_per_hz = u.fft_size / u.sample_rate;
    let half_oct = half_octaves_at(freq);
    if (half_oct <= 0.0) {
        return sample_bin_db_side(freq * bins_per_hz, num_bins);
    }
    let bin_lo_f = clamp(freq * exp2(-half_oct) * bins_per_hz, 0.0, num_bins - 1.0);
    let bin_hi_f = clamp(freq * exp2(half_oct) * bins_per_hz, 0.0, num_bins - 1.0);
    let span_bins = max(bin_hi_f - bin_lo_f, 1e-6);
    let step = span_bins / f32(SMOOTH_N_MAX);
    var power_sum = 0.0;
    for (var i: i32 = 0; i < SMOOTH_N_MAX; i = i + 1) {
        let b_f = bin_lo_f + (f32(i) + 0.5) * step;
        let db = sample_bin_db_side(b_f, num_bins);
        power_sum = power_sum + pow(10.0, db * 0.1);
    }
    return 10.0 * log(power_sum / f32(SMOOTH_N_MAX) + 1e-24) / log(10.0);
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

// Vision 4X "Heatmap" style jet — black → navy → blue → cyan → green →
// yellow → red → white. The top 10% goes red→white so loudest peaks
// clearly separate from merely-loud content (solves jet's classic
// red-vs-darker-red crush at the top end).
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

    // Y axis: log-spaced. rel_y=0 → top = freq_max; rel_y=1 → bottom =
    // freq_min. Map the user-selected freq range onto the CQT's stored
    // log bins (cqt_fmin_hz + bins_per_octave) so zooming the curves
    // zooms the spectrogram too.
    let spec_y = px.y - u.spectrum_height;
    let spec_h = max(u.resolution.y - u.spectrum_height, 1.0);
    let rel_y = clamp(spec_y / spec_h, 0.0, 1.0);
    let log_min = log(u.freq_min);
    let log_max = log(u.freq_max);
    let log_freq = mix(log_max, log_min, rel_y);
    let freq = exp(log_freq);
    let log_bin_f = clamp(
        u.cqt_bins_per_octave * log2(freq / u.cqt_fmin_hz),
        0.0,
        f32(log_bins_i) - 1.0,
    );

    // X axis: free mode scrolls newest→right, older→left. Sync mode maps
    // pixel x onto [0, history_cols) so columns stay pinned to the beat
    // grid and the write position advances left→right, wrapping to
    // overwrite the oldest pixels — matches Vision 4X's synced spectrogram.
    //
    // Free mode is strictly 1 history column = 1 screen pixel, so integer
    // snap is exact — no interp needed. Sync mode stretches history_cols
    // across the screen width; when cols-per-pixel < 1 we'd see stairs
    // without sub-pixel blending, so lerp adjacent columns in the power
    // domain (matches the Y-axis behaviour in sample_history_db).
    var raw_db: f32;
    if (u.sync_mode > 0.5) {
        let rel_x = clamp(px.x / max(u.resolution.x, 1.0), 0.0, 0.9999);
        let col_f = rel_x * f32(history_cols_i);
        let col_lo = clamp(i32(floor(col_f)), 0, history_cols_i - 1);
        let col_hi = min(col_lo + 1, history_cols_i - 1);
        let frac_x = col_f - f32(col_lo);
        let db_lo = sample_history_db(col_lo, log_bin_f, log_bins_i);
        let db_hi = sample_history_db(col_hi, log_bin_f, log_bins_i);
        let p_lo = pow(10.0, db_lo * 0.1);
        let p_hi = pow(10.0, db_hi * 0.1);
        let p = mix(p_lo, p_hi, frac_x);
        raw_db = 10.0 * log(p + 1e-24) / log(10.0);
    } else {
        let history_idx = i32(floor(u.resolution.x - 1.0 - px.x));
        if (history_idx < 0 || history_idx >= history_cols_i) {
            return vec4<f32>(0.0);
        }
        var c = write_col_i - history_idx;
        c = ((c % history_cols_i) + history_cols_i) % history_cols_i;
        raw_db = sample_history_db(c, log_bin_f, log_bins_i);
    }

    // Run the same weighting used on the curves so the colourmap reads
    // perceptually (K-weighting reveals what's driving loudness; linear
    // tilts give SPAN-style HF brightness).
    let weighted_db = tilt_db(freq, raw_db);
    let span = max(u.spectrogram_db_max - u.spectrogram_db_min, 1e-3);
    let t_lin = clamp((weighted_db - u.spectrogram_db_min) / span, 0.0, 1.0);
    // Gamma on the colour encoding (not on the stored values) lifts quiet
    // detail into the visible band without washing out peaks. gamma < 1
    // brightens, gamma > 1 darkens; gamma == 1 is pass-through.
    let gamma = max(u.spectrogram_gamma, 1e-3);
    let t = pow(t_lin, gamma);
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
