// Spectrum line — fullscreen fragment shader.
//
// For each output pixel we sample the dB at three log-frequency positions
// (this pixel's left edge, center, and right edge) and evaluate the
// distance to the two line segments joining them. This gives a visually
// connected line even when neighbouring pixels have very different y
// values (steep spectral slopes), which the naive "sample once, check
// vertical distance" approach renders as disconnected dots.

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
    line_thickness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> spectrum: array<f32>;

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
    return u.resolution.y * (1.0 - clamp(t, 0.0, 1.0));
}

fn y_px_at_freq(freq: f32) -> f32 {
    let num_bins = u.fft_size * 0.5;
    let bin_f = freq * u.fft_size / u.sample_rate;
    if (bin_f < 0.0 || bin_f > num_bins - 1.0) {
        return db_to_y_px(u.db_min);
    }
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, u32(num_bins) - 1u);
    let frac = fract(bin_f);
    let db = mix(spectrum[bin_lo], spectrum[bin_hi], frac);
    return db_to_y_px(db);
}

fn sdf_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.uv * u.resolution;

    let log_lo = log(u.freq_min);
    let log_hi = log(u.freq_max);
    let log_range = log_hi - log_lo;
    let dx_per_px = log_range / u.resolution.x;
    let log_f = log_lo + in.uv.x * log_range;

    let y_prev = y_px_at_freq(exp(log_f - dx_per_px));
    let y_curr = y_px_at_freq(exp(log_f));
    let y_next = y_px_at_freq(exp(log_f + dx_per_px));

    let a = vec2<f32>(px.x - 1.0, y_prev);
    let b = vec2<f32>(px.x,       y_curr);
    let c = vec2<f32>(px.x + 1.0, y_next);

    let d = min(sdf_segment(px, a, b), sdf_segment(px, b, c));

    let half_t = u.line_thickness * 0.5;
    let aa = 1.0 - smoothstep(half_t - 0.5, half_t + 0.5, d);

    return mix(u.bg_color, u.line_color, aa);
}
