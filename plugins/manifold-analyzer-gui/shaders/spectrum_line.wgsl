// Spectrum + spectrogram — fullscreen fragment shader.
//
// Top region (y < spectrum_height):
//   Mid + Side line/fill curves. Pipeline:
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
    freq_min: f32,
    freq_max: f32,
    db_min: f32,
    db_max: f32,
    line_color: vec4<f32>,
    bg_color: vec4<f32>,
    side_color: vec4<f32>,
    line_thickness: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    // Number of valid entries in `mid_spectrum` / `side_spectrum`
    // (= phys_w of the spectrum region). The MS curves are pre-smoothed
    // CPU-side at one entry per output pixel column, so the fragment
    // shader is a pure column lookup — no per-pixel BH-window loop.
    spectrum_columns: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
    sync_mode: f32,
    cqt_fmin_hz: f32,
    cqt_bins_per_octave: f32,
    spectrogram_gamma: f32,
    // 0 = single full-height spectrogram from `history` (Mid or Side).
    // 1 = stacked L+R: top half samples `history` (Left), bottom half
    //     samples `history2` (Right). The split lives in the shader so
    //     the CPU side can flip it at runtime without re-binding.
    spectrogram_mode: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
// Per-bin dB values: weighting (Pink / Tilted / LUFS / etc.) is already
// applied CPU-side before upload, so the shader can stay completely
// tilt-agnostic. Earlier revisions used a binding=4 weighting LUT here
// and called `tilt_db = raw + lut(freq)` per pixel; the LUT path was
// removed because applying tilt per-bin then smoothing is both simpler
// and slightly more correct than the shader's smooth-then-tilt-at-
// centre approach.
@group(0) @binding(1) var<storage, read> mid_spectrum: array<f32>;
@group(0) @binding(2) var<storage, read> side_spectrum: array<f32>;
@group(0) @binding(3) var<storage, read> history: array<f32>;
// Secondary spectrogram history. Always bound; only read when
// `u.spectrogram_mode > 0.5` (L+R stacked), otherwise the shader's
// branch ignores it and the buffer stays at the silence floor.
@group(0) @binding(5) var<storage, read> history2: array<f32>;

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
    // Guard inverted/zero range — without max(), a misconfigured
    // db_max == db_min would produce NaN/inf and snap pixels to the
    // edge non-deterministically.
    let span = max(u.db_max - u.db_min, 1e-3);
    let t = (db - u.db_min) / span;
    return u.spectrum_height * (1.0 - clamp(t, 0.0, 1.0));
}

// MS curves: the spectrum buffers hold one already-smoothed,
// already-weighted dB value per output pixel column. Fragment shader
// reads three adjacent columns (prev / curr / next) for SDF line AA
// — no smoothing, no tilt, no FFT-bin math.
fn mid_at_column(col: i32) -> f32 {
    let n = i32(u.spectrum_columns);
    if (n <= 0) {
        return u.db_min;
    }
    let c = clamp(col, 0, n - 1);
    return mid_spectrum[c];
}

fn side_at_column(col: i32) -> f32 {
    let n = i32(u.spectrum_columns);
    if (n <= 0) {
        return u.db_min;
    }
    let c = clamp(col, 0, n - 1);
    return side_spectrum[c];
}

fn sdf_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

// Heatmap-style jet ramp — black → navy → blue → cyan → green →
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

// Same lookup against the secondary spectrogram buffer (Right channel
// in L+R stacked mode). WGSL doesn't allow taking storage-array
// references as parameters, so the two paths are duplicated rather
// than parameterised. Kept identical to `sample_history_db` so any
// future fix lands in both.
fn sample_history2_db(col: i32, log_bin_f: f32, log_bins_i: i32) -> f32 {
    let clamped = clamp(log_bin_f, 0.0, f32(log_bins_i) - 1.0);
    let lo = i32(floor(clamped));
    let hi = min(lo + 1, log_bins_i - 1);
    let frac = fract(clamped);
    let db_lo = history2[col * log_bins_i + lo];
    let db_hi = history2[col * log_bins_i + hi];
    let p_lo = pow(10.0, db_lo * 0.1);
    let p_hi = pow(10.0, db_hi * 0.1);
    let p = mix(p_lo, p_hi, frac);
    return 10.0 * log(p + 1e-24) / log(10.0);
}

// Per-pixel raw dB lookup against the supplied history buffer (Buffer 0
// = primary `history`, Buffer 1 = secondary `history2`). Keeps the
// shared X-axis math (free vs sync mode) in one place; the Y-axis
// log-bin position and the colourmap step both stay in
// `spectrogram_region` so we don't pay extra log2/pow per channel.
fn spectrogram_raw_db(
    buffer_id: i32,
    log_bin_f: f32,
    log_bins_i: i32,
    px_x: f32,
    history_cols_i: i32,
    write_col_i: i32,
) -> f32 {
    if (u.sync_mode > 0.5) {
        let rel_x = clamp(px_x / max(u.resolution.x, 1.0), 0.0, 0.9999);
        let col_f = rel_x * f32(history_cols_i);
        let col_lo = clamp(i32(floor(col_f)), 0, history_cols_i - 1);
        let col_hi = min(col_lo + 1, history_cols_i - 1);
        let frac_x = col_f - f32(col_lo);
        var db_lo: f32;
        var db_hi: f32;
        if (buffer_id == 0) {
            db_lo = sample_history_db(col_lo, log_bin_f, log_bins_i);
            db_hi = sample_history_db(col_hi, log_bin_f, log_bins_i);
        } else {
            db_lo = sample_history2_db(col_lo, log_bin_f, log_bins_i);
            db_hi = sample_history2_db(col_hi, log_bin_f, log_bins_i);
        }
        let p_lo = pow(10.0, db_lo * 0.1);
        let p_hi = pow(10.0, db_hi * 0.1);
        let p = mix(p_lo, p_hi, frac_x);
        return 10.0 * log(p + 1e-24) / log(10.0);
    }
    // Free-scroll: 1 history column = 1 screen pixel, integer-snapped
    // for bit-stable edges under fractional DPI scaling.
    let res_w_i = i32(u.resolution.x);
    let px_col = i32(floor(px_x));
    let history_idx = (res_w_i - 1) - px_col;
    if (history_idx < 0 || history_idx >= history_cols_i) {
        return -1000.0;
    }
    var c = write_col_i - history_idx;
    c = ((c % history_cols_i) + history_cols_i) % history_cols_i;
    if (buffer_id == 0) {
        return sample_history_db(c, log_bin_f, log_bins_i);
    }
    return sample_history2_db(c, log_bin_f, log_bins_i);
}

fn raw_to_color(freq: f32, raw_db: f32) -> vec4<f32> {
    if (raw_db < -999.0) {
        return vec4<f32>(0.0);
    }
    // `raw_db` is already weighted (CPU pre-tilts each CQT column on
    // arrival from the worker), so it goes straight into the colourmap.
    let span = max(u.spectrogram_db_max - u.spectrogram_db_min, 1e-3);
    let t_lin = clamp((raw_db - u.spectrogram_db_min) / span, 0.0, 1.0);
    let gamma = max(u.spectrogram_gamma, 1e-3);
    let t = pow(t_lin, gamma);
    let rgb = colormap(t);
    return vec4<f32>(rgb, 1.0);
}

fn spectrogram_pixel(px: vec2<f32>) -> vec4<f32> {
    let history_cols_i = i32(u.history_cols);
    let write_col_i = i32(u.write_col);
    let log_bins_i = i32(u.log_bins);

    let spec_y = px.y - u.spectrum_height;
    let spec_h = max(u.resolution.y - u.spectrum_height, 1.0);

    // Stacked L+R mode: split the spectrogram into top half (Left) and
    // bottom half (Right). Each half remaps to the full freq range so
    // both channels share the same vertical axis — squashed half-height
    // but the bands still line up between L and R for direct compare.
    var sub_y: f32;
    var sub_h: f32;
    var buffer_id: i32;
    if (u.spectrogram_mode > 0.5) {
        let half = spec_h * 0.5;
        if (spec_y < half) {
            sub_y = spec_y;
            sub_h = max(half, 1.0);
            buffer_id = 0;
        } else {
            sub_y = spec_y - half;
            sub_h = max(spec_h - half, 1.0);
            buffer_id = 1;
        }
    } else {
        sub_y = spec_y;
        sub_h = spec_h;
        buffer_id = 0;
    }

    // Y axis: log-spaced inside whichever sub-region we landed in. rel_y=0
    // → top of sub-region = freq_max; rel_y=1 → bottom = freq_min. Map
    // the user-selected freq range onto the CQT's stored log bins
    // (cqt_fmin_hz + bins_per_octave) so zooming the curves zooms the
    // spectrogram too.
    let rel_y = clamp(sub_y / sub_h, 0.0, 1.0);
    let log_min = log(u.freq_min);
    let log_max = log(u.freq_max);
    let log_freq = mix(log_max, log_min, rel_y);
    let freq = exp(log_freq);
    let log_bin_f = clamp(
        u.cqt_bins_per_octave * log2(freq / u.cqt_fmin_hz),
        0.0,
        f32(log_bins_i) - 1.0,
    );

    let raw_db = spectrogram_raw_db(
        buffer_id,
        log_bin_f,
        log_bins_i,
        px.x,
        history_cols_i,
        write_col_i,
    );
    return raw_to_color(freq, raw_db);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.uv * u.resolution;

    // Spectrogram region — opaque fill, no curves. Uses its own freq
    // axis (log-y) inside `spectrogram_pixel`.
    if (px.y >= u.spectrum_height) {
        return spectrogram_pixel(px);
    }

    // Spectrum region — pure column lookup. CPU per-column smoothing
    // means three adjacent column reads give us prev/curr/next anchors
    // for the SDF line AA, no per-pixel work required.
    let col_curr = i32(px.x);
    let col_prev = col_curr - 1;
    let col_next = col_curr + 1;

    let my_prev = db_to_y_px(mid_at_column(col_prev));
    let my_curr = db_to_y_px(mid_at_column(col_curr));
    let my_next = db_to_y_px(mid_at_column(col_next));
    let ma = vec2<f32>(px.x - 1.0, my_prev);
    let mb = vec2<f32>(px.x,       my_curr);
    let mc = vec2<f32>(px.x + 1.0, my_next);
    let dm = min(sdf_segment(px, ma, mb), sdf_segment(px, mb, mc));

    let sy_prev = db_to_y_px(side_at_column(col_prev));
    let sy_curr = db_to_y_px(side_at_column(col_curr));
    let sy_next = db_to_y_px(side_at_column(col_next));
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
