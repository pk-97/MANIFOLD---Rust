//! Offline feature-grading harness for audio modulation.
//!
//! Decodes an audio clip, runs the EXACT live analysis path
//! ([`StreamingSendAnalyzer`]) causally hop-by-hop, and renders one PNG:
//! spectrogram (with band dividers, per-band centroid traces, onset ticks) on
//! top, per-feature lanes below (raw hop-rate value dim, default-shape
//! smoothed value bright). The eyes-as-oracle loop: Peter listens to the clip,
//! looks at the picture, and marks where the lines betray his ears. Every
//! change to the analysis core gets graded here against the same reference
//! clips before it ships to the live path.
//!
//! ```text
//! cargo run -p manifold-audio --example mod_harness -- <clip.(wav|aiff|mp3|flac)> \
//!     [--out out.png] [--low 250] [--mid 2000] [--floor -120] [--start s] [--dur s]
//! cargo run -p manifold-audio --example mod_harness -- --selftest [--out out.png]
//! ```
//!
//! `--selftest` synthesizes a known signal (supersaw pitch dive → wobble bass →
//! busy mix, kicks throughout) so the harness verifies itself without a clip:
//! the centroid trace must follow the dive, the amplitude lane must oscillate
//! at the wobble rate, and the transients lane must tick with the kicks.
//!
//! Also prints a per-feature jitter index (mean |Δ| per hop, raw vs smoothed)
//! so successive analysis iterations can be compared with a number as well as
//! by eye.

use manifold_audio::analysis::StreamingSendAnalyzer;
use manifold_core::audio_mod::AudioModShape;
use manifold_core::audio_setup::{DEFAULT_LOW_HZ, DEFAULT_MID_HZ, FLOOR_DB_OFF};
use manifold_spectral::SpectrogramConfig;

const BAND_NAMES: [&str; 4] = ["FULL", "LOW", "MID", "HIGH"];
const BAND_COLORS: [[u8; 3]; 4] = [
    [235, 235, 235], // Full — white
    [255, 92, 92],   // Low — red
    [92, 224, 112],  // Mid — green
    [96, 156, 255],  // High — blue
];
const FEATURE_NAMES: [&str; 5] = ["AMPLITUDE", "BRIGHTNESS", "NOISINESS", "LIVELINESS", "TRANSIENTS"];

/// One hop's worth of everything we plot.
struct HopRecord {
    /// Raw spectrogram column (untilted magnitudes), `num_bins` long.
    col: Vec<f32>,
    /// Per-band centroid height-from-bottom 0..1, -1 = hidden. [Full, Low, Mid, High].
    centroid_yfb: [f32; 4],
    /// Per-band onset fire flags (1.0 on the fired hop). [Low, Mid, High].
    onset_fired: [f32; 3],
    /// features[feature][band], feature order per FEATURE_NAMES, band order per BAND_NAMES.
    raw: [[f32; 4]; 5],
    /// Same, after the default AudioModShape follower (what a param would receive).
    smoothed: [[f32; 4]; 5],
}

struct Args {
    input: Option<String>,
    out: String,
    low_hz: f32,
    mid_hz: f32,
    floor_db: f32,
    start_s: f32,
    dur_s: f32,
    selftest: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: None,
        out: String::new(),
        low_hz: DEFAULT_LOW_HZ,
        mid_hz: DEFAULT_MID_HZ,
        floor_db: FLOOR_DB_OFF,
        start_s: 0.0,
        dur_s: f32::INFINITY,
        selftest: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let next = |i: &mut usize| -> Result<String, String> {
        *i += 1;
        argv.get(*i).cloned().ok_or_else(|| format!("missing value after {}", argv[*i - 1]))
    };
    while i < argv.len() {
        match argv[i].as_str() {
            "--out" => args.out = next(&mut i)?,
            "--low" => args.low_hz = next(&mut i)?.parse().map_err(|e| format!("--low: {e}"))?,
            "--mid" => args.mid_hz = next(&mut i)?.parse().map_err(|e| format!("--mid: {e}"))?,
            "--floor" => args.floor_db = next(&mut i)?.parse().map_err(|e| format!("--floor: {e}"))?,
            "--start" => args.start_s = next(&mut i)?.parse().map_err(|e| format!("--start: {e}"))?,
            "--dur" => args.dur_s = next(&mut i)?.parse().map_err(|e| format!("--dur: {e}"))?,
            "--selftest" => args.selftest = true,
            s if s.starts_with("--") => return Err(format!("unknown flag {s}")),
            s => args.input = Some(s.to_string()),
        }
        i += 1;
    }
    if !args.selftest && args.input.is_none() {
        return Err("usage: mod_harness <clip> [--out out.png] [--low hz] [--mid hz] [--floor db] [--start s] [--dur s] | --selftest".into());
    }
    if args.out.is_empty() {
        args.out = match &args.input {
            Some(p) => format!("{}.features.png", p.trim_end_matches(|c| c != '.').trim_end_matches('.')),
            None => "mod_harness_selftest.png".into(),
        };
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // ── Source: decode file (downmix all channels, same mean the live path uses)
    //    or synthesize the self-test signal. Mono at `sr`.
    let (mono, sr, label) = if args.selftest {
        let (m, sr) = synth_selftest();
        (m, sr, "selftest".to_string())
    } else {
        let path = args.input.clone().unwrap();
        let decoded = match manifold_playback::audio_decoder::decode_audio_to_pcm(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        let ch = decoded.channels.max(1);
        let mono: Vec<f32> = decoded
            .samples
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect();
        (mono, decoded.sample_rate, path)
    };

    // Optional excerpt.
    let start = ((args.start_s.max(0.0) * sr as f32) as usize).min(mono.len());
    let len = if args.dur_s.is_finite() {
        ((args.dur_s.max(0.0) * sr as f32) as usize).min(mono.len() - start)
    } else {
        mono.len() - start
    };
    let mono = &mono[start..start + len];
    if mono.is_empty() {
        eprintln!("empty excerpt (start/dur out of range)");
        std::process::exit(1);
    }

    // ── Analysis: the exact live path, fed causally one hop at a time so
    //    `latest()` is sampled at hop rate — what the modulation evaluator sees.
    let cfg = SpectrogramConfig::default();
    let hop = cfg.hop.max(1);
    let dt = hop as f32 / sr as f32;
    let mut an = StreamingSendAnalyzer::new(sr, args.low_hz, args.mid_hz);
    an.set_floor_db(args.floor_db);
    an.set_scope(true);
    let num_bins = an.num_bins();
    let (fmin, fmax) = an.freq_range();

    let shape = AudioModShape::default();
    let mut smooth_state = [[0.0f32; 4]; 5];
    let mut prev_raw = [[0.0f32; 4]; 5];
    let mut records: Vec<HopRecord> = Vec::with_capacity(mono.len() / hop + 1);

    for chunk in mono.chunks(hop) {
        an.push(chunk);
        let mut cols: Vec<Vec<f32>> = Vec::new();
        an.drain_scope_columns(|c| cols.push(c.to_vec()));
        let mut scalars: Vec<([f32; 4], [f32; 3])> = Vec::new();
        an.drain_scope_scalars(|c, o| scalars.push((c, o)));
        let f = an.latest();
        // One hop in → one column out (a short final chunk emits none).
        for (col, (centroid, onsets)) in cols.into_iter().zip(scalars) {
            let mut raw = [[0.0f32; 4]; 5];
            let mut smoothed = [[0.0f32; 4]; 5];
            for b in 0..4 {
                let bf = &f.bands[b];
                let vals = [bf.amplitude, bf.brightness, bf.noisiness, bf.liveliness, bf.transients];
                for (fi, &v) in vals.iter().enumerate() {
                    raw[fi][b] = v;
                    smoothed[fi][b] =
                        shape.apply(v, dt, &mut smooth_state[fi][b], &mut prev_raw[fi][b]);
                }
            }
            records.push(HopRecord { col, centroid_yfb: centroid, onset_fired: onsets, raw, smoothed });
        }
    }

    if records.is_empty() {
        eprintln!("clip shorter than one hop ({hop} samples); nothing to draw");
        std::process::exit(1);
    }

    // ── Jitter index: mean |Δ| per hop, raw vs smoothed. A number to watch
    //    across analysis iterations alongside the picture.
    println!(
        "{label}: {:.2}s @ {sr} Hz, {} hops of {hop} samples ({:.2} ms), {num_bins} bins {:.0}-{:.0} Hz",
        mono.len() as f32 / sr as f32,
        records.len(),
        1000.0 * dt,
        fmin,
        fmax
    );
    println!("jitter = mean |delta| per hop (raw -> smoothed), per band:");
    for (fi, name) in FEATURE_NAMES.iter().enumerate() {
        let mut line = format!("  {name:<11}");
        for (b, band_name) in BAND_NAMES.iter().enumerate() {
            let (mut jr, mut js) = (0.0f64, 0.0f64);
            for w in records.windows(2) {
                jr += (w[1].raw[fi][b] - w[0].raw[fi][b]).abs() as f64;
                js += (w[1].smoothed[fi][b] - w[0].smoothed[fi][b]).abs() as f64;
            }
            let n = (records.len() - 1).max(1) as f64;
            line += &format!("  {band_name}: {:.4}->{:.4}", jr / n, js / n);
        }
        println!("{line}");
    }

    render_png(&args.out, &records, &cfg, sr, args.low_hz, args.mid_hz);
    println!("wrote {}", args.out);
}

// ── Self-test signal ─────────────────────────────────────────────────────

/// 8 s @ 48 kHz: 0-3 s supersaw dive 1200→150 Hz; 3-5.5 s wobble bass (150 Hz,
/// 3 Hz amplitude LFO); 5.5-8 s busy mix (saw + noise pad); kick every 0.5 s
/// throughout. Exercises the three things the picture must show: a centroid
/// trace following the dive, an amplitude lane oscillating at 3 Hz, and
/// transient ticks at 2 Hz.
fn synth_selftest() -> (Vec<f32>, u32) {
    let sr = 48_000u32;
    let n = 8 * sr as usize;
    let mut out = vec![0.0f32; n];
    let srf = sr as f32;

    // Supersaw: 7 naive saws detuned ±12 cents (aliasing is irrelevant for eval).
    let detunes: [f32; 7] = [-0.12, -0.08, -0.04, 0.0, 0.04, 0.08, 0.12];
    let mut phases = [0.0f32; 7];
    for (i, s_out) in out.iter_mut().take(3 * sr as usize).enumerate() {
        let t = i as f32 / srf;
        let f0 = 1200.0 * (150.0f32 / 1200.0).powf(t / 3.0); // exponential glide
        let mut s = 0.0;
        for (p, det) in phases.iter_mut().zip(detunes) {
            let f = f0 * 2.0f32.powf(det / 12.0);
            *p = (*p + f / srf).fract();
            s += 2.0 * *p - 1.0;
        }
        *s_out += 0.12 * s;
    }

    // Wobble bass: 150 Hz saw, amplitude LFO at 3 Hz.
    let mut p = 0.0f32;
    let wobble_start = 3 * sr as usize;
    let wobble_end = 5 * sr as usize + sr as usize / 2;
    for (j, s_out) in out[wobble_start..wobble_end].iter_mut().enumerate() {
        let t = (wobble_start + j) as f32 / srf;
        p = (p + 150.0 / srf).fract();
        let lfo = 0.5 + 0.5 * (std::f32::consts::TAU * 3.0 * t).sin();
        *s_out += 0.5 * lfo * (2.0 * p - 1.0);
    }

    // Busy mix: saw + white-ish noise pad (LCG noise).
    let mut seed = 0x2545F491u32;
    let mut p2 = 0.0f32;
    for s_out in out[wobble_end..].iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
        p2 = (p2 + 220.0 / srf).fract();
        *s_out += 0.25 * (2.0 * p2 - 1.0) + 0.15 * noise;
    }

    // Kicks: every 0.5 s, 90 ms pitch-swept sine 120→45 Hz with exp decay.
    for k in 0..16 {
        let start = k * sr as usize / 2;
        let mut ph = 0.0f32;
        for j in 0..(sr as usize * 9 / 100) {
            let idx = start + j;
            if idx >= n {
                break;
            }
            let tj = j as f32 / srf;
            let f = 120.0 * (45.0f32 / 120.0).powf(tj / 0.09);
            ph += f / srf;
            out[idx] += 0.8 * (-tj / 0.03).exp() * (std::f32::consts::TAU * ph).sin();
        }
    }

    // Soft clip to keep it sane.
    for s in &mut out {
        *s = s.tanh();
    }
    (out, sr)
}

// ── Rendering ────────────────────────────────────────────────────────────

const MAX_PLOT_W: usize = 4096;
const SPEC_H: usize = 340;
const LANE_H: usize = 56;
const LANE_LABEL_H: usize = 12;
const LANE_GAP: usize = 6;
const AXIS_H: usize = 18;
const MARGIN: usize = 10;
const TITLE_H: usize = 16;

fn render_png(
    path: &str,
    records: &[HopRecord],
    cfg: &SpectrogramConfig,
    sr: u32,
    low_hz: f32,
    mid_hz: f32,
) {
    // Both derivable from cfg + sr; recomputing beats threading them through.
    let num_bins = cfg.num_bins(sr as f32).max(1);
    let dt = cfg.hop.max(1) as f32 / sr as f32;
    // Decimate hops to pixel buckets. Spectrogram takes the bucket MAX per bin
    // (peaks stay visible); feature traces draw the bucket's min..max span so
    // decimation can never hide the jitter this harness exists to expose.
    let n = records.len();
    let w = n.min(MAX_PLOT_W);
    let bucket = |x: usize| -> (usize, usize) {
        let lo = x * n / w;
        let hi = ((x + 1) * n / w).max(lo + 1).min(n);
        (lo, hi)
    };

    let lanes = FEATURE_NAMES.len();
    let img_w = w + 2 * MARGIN;
    let img_h = TITLE_H + SPEC_H + LANE_GAP + lanes * (LANE_LABEL_H + LANE_H + LANE_GAP) + AXIS_H + 2 * MARGIN;
    let mut img = image::RgbImage::from_pixel(img_w as u32, img_h as u32, image::Rgb([12, 12, 16]));

    let x0 = MARGIN;
    let mut y = MARGIN;

    // Title: config + legend.
    draw_text(&mut img, x0, y, &format!(
        "SPECTROGRAM {:.0}-{:.0}HZ  LOW<{:.0}  MID<{:.0}  HOP {:.1}MS   LANES: RAW DIM / SMOOTHED BRIGHT   FULL LOW MID HIGH",
        cfg.fmin,
        cfg.effective_fmax(sr as f32),
        low_hz,
        mid_hz,
        dt * 1000.0
    ), [180, 180, 190]);
    // Legend swatches after the text (positions chosen to sit over the trailing words).
    y += TITLE_H;

    // ── Spectrogram: tilted dB through a 5-stop ramp, bin 0 at the bottom.
    let tilt_range = (cfg.effective_fmax(sr as f32) / cfg.fmin.max(1.0)).log2();
    let inv_nb = if num_bins > 1 { 1.0 / (num_bins - 1) as f32 } else { 0.0 };
    for x in 0..w {
        let (lo, hi) = bucket(x);
        for py in 0..SPEC_H {
            // Pixel row → fractional bin (bottom = bin 0).
            let bin_f = (SPEC_H - 1 - py) as f32 / (SPEC_H - 1) as f32 * (num_bins - 1) as f32;
            let bin = bin_f as usize;
            let mut mag = 0.0f32;
            for r in &records[lo..hi] {
                mag = mag.max(r.col[bin.min(num_bins - 1)]);
            }
            let v = if mag <= 0.0 {
                0.0
            } else {
                let tilt_db = cfg.tilt_slope * tilt_range * (bin as f32 * inv_nb - 0.5);
                let db = 20.0 * mag.max(1e-9).log10() + tilt_db;
                ((db - cfg.db_min) / (cfg.db_max - cfg.db_min)).clamp(0.0, 1.0)
            };
            img.put_pixel((x0 + x) as u32, (y + py) as u32, image::Rgb(colormap(v)));
        }
    }
    // Band divider lines at the same geometric mapping the analysis uses.
    let bin_of = |hz: f32| -> usize {
        ((cfg.bpo as f32 * (hz / cfg.fmin.max(1.0)).max(1e-6).log2()).round() as i64)
            .clamp(1, num_bins as i64 - 1) as usize
    };
    for hz in [low_hz, mid_hz] {
        let py = SPEC_H - 1 - (bin_of(hz) as f32 * inv_nb * (SPEC_H - 1) as f32) as usize;
        for x in 0..w {
            blend_pixel(&mut img, x0 + x, y + py, [255, 255, 255], 0.25);
        }
    }
    // Centroid traces (band colors) + onset ticks (top of spectrogram, band color).
    for x in 0..w {
        let (lo, hi) = bucket(x);
        let r = &records[lo]; // trace: first record of bucket is fine at this density
        for (&cy, &color) in r.centroid_yfb.iter().zip(BAND_COLORS.iter()) {
            if cy >= 0.0 {
                let py = SPEC_H - 1 - (cy * (SPEC_H - 1) as f32) as usize;
                blend_pixel(&mut img, x0 + x, y + py, color, 0.9);
            }
        }
        for oi in 0..3 {
            if records[lo..hi].iter().any(|rec| rec.onset_fired[oi] > 0.5) {
                for py in 0..8 {
                    blend_pixel(&mut img, x0 + x, y + 2 + oi * 9 + py, BAND_COLORS[oi + 1], 0.9);
                }
            }
        }
    }
    y += SPEC_H + LANE_GAP;

    // ── Feature lanes.
    for (fi, name) in FEATURE_NAMES.iter().enumerate() {
        draw_text(&mut img, x0, y, name, [150, 150, 160]);
        y += LANE_LABEL_H;
        // Lane background + gridlines at 0.5.
        for py in 0..LANE_H {
            for x in 0..w {
                let base = if py == LANE_H / 2 { [30, 30, 38] } else { [18, 18, 24] };
                img.put_pixel((x0 + x) as u32, (y + py) as u32, image::Rgb(base));
            }
        }
        for (b, &color) in BAND_COLORS.iter().enumerate() {
            for x in 0..w {
                let (lo, hi) = bucket(x);
                // Raw: min..max span, dim — the honest jitter.
                let (mut mn, mut mx) = (f32::MAX, f32::MIN);
                for r in &records[lo..hi] {
                    mn = mn.min(r.raw[fi][b]);
                    mx = mx.max(r.raw[fi][b]);
                }
                let py_of = |v: f32| LANE_H - 1 - ((v.clamp(0.0, 1.0)) * (LANE_H - 1) as f32) as usize;
                for py in py_of(mx)..=py_of(mn) {
                    blend_pixel(&mut img, x0 + x, y + py, color, 0.22);
                }
                // Smoothed: single bright trace (bucket mean).
                let mut sm = 0.0;
                for r in &records[lo..hi] {
                    sm += r.smoothed[fi][b];
                }
                let sm = sm / (hi - lo) as f32;
                blend_pixel(&mut img, x0 + x, y + py_of(sm), color, 0.95);
            }
        }
        y += LANE_H + LANE_GAP;
    }

    // ── Time axis: tick + label every second.
    let hops_per_sec = 1.0 / dt;
    let mut sec = 0usize;
    loop {
        let hop_idx = (sec as f32 * hops_per_sec) as usize;
        if hop_idx >= n {
            break;
        }
        let x = hop_idx * w / n;
        for py in 0..5 {
            blend_pixel(&mut img, x0 + x, y + py, [200, 200, 210], 0.8);
        }
        draw_text(&mut img, x0 + x + 2, y + 6, &format!("{sec}S"), [140, 140, 150]);
        sec += 1;
    }

    img.save(path).unwrap_or_else(|e| {
        eprintln!("failed to write {path}: {e}");
        std::process::exit(1);
    });
}

/// 5-stop magma-ish ramp: black → deep purple → magenta → orange → near-white.
fn colormap(v: f32) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.00, [0.0, 0.0, 0.0]),
        (0.25, [45.0, 15.0, 80.0]),
        (0.50, [160.0, 40.0, 130.0]),
        (0.75, [245.0, 125.0, 50.0]),
        (1.00, [252.0, 245.0, 210.0]),
    ];
    let v = v.clamp(0.0, 1.0);
    for pair in STOPS.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if v <= t1 {
            let t = (v - t0) / (t1 - t0);
            return [
                (c0[0] + (c1[0] - c0[0]) * t) as u8,
                (c0[1] + (c1[1] - c0[1]) * t) as u8,
                (c0[2] + (c1[2] - c0[2]) * t) as u8,
            ];
        }
    }
    [252, 245, 210]
}

fn blend_pixel(img: &mut image::RgbImage, x: usize, y: usize, color: [u8; 3], alpha: f32) {
    if x as u32 >= img.width() || y as u32 >= img.height() {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    for (chan, &c) in p.0.iter_mut().zip(color.iter()) {
        *chan = (*chan as f32 * (1.0 - alpha) + c as f32 * alpha) as u8;
    }
}

// ── Minimal 5x7 pixel font (uppercase, digits, few symbols) ──────────────

fn draw_text(img: &mut image::RgbImage, x: usize, y: usize, text: &str, color: [u8; 3]) {
    let mut cx = x;
    for ch in text.chars() {
        if let Some(glyph) = glyph5x7(ch.to_ascii_uppercase()) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0b10000 >> col) != 0 {
                        blend_pixel(img, cx + col, y + row, color, 1.0);
                    }
                }
            }
        }
        cx += 6;
    }
}

/// Row-major 5-bit rows, top to bottom. None = space / unsupported.
fn glyph5x7(c: char) -> Option<[u8; 7]> {
    Some(match c {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110],
        ':' => [0b00000, 0b00110, 0b00110, 0b00000, 0b00110, 0b00110, 0b00000],
        '-' => [0b00000, 0b00000, 0b00000, 0b01110, 0b00000, 0b00000, 0b00000],
        '<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        _ => return None,
    })
}
