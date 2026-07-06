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
//!     [--out out.png] [--low 250] [--mid 2000] [--floor -120] [--start s] [--dur s] \
//!     [--csv dir]
//! cargo run -p manifold-audio --example mod_harness -- --selftest [--out out.png] [--csv dir]
//! ```
//!
//! `--selftest` synthesizes six known scenarios, ONE PNG EACH (suffixes
//! `_dive`, `_wobble`, `_kicks`, `_busymix`, `_riser`, `_growl`), so the
//! harness verifies itself without a clip: the centroid trace must follow the
//! dive, the amplitude lane must oscillate at the wobble rate, the transients
//! lane must tick with the kicks, the riser must show a swept noise band with
//! no stable harmonic peak (the presence-null case), and the growl must show
//! a fixed 150 Hz fundamental with a moving spectral tilt (the constant-pitch
//! case). The spectrogram is drawn with the scope shader's exact display
//! transform and jet colormap (ported from `spectrogram.wgsl`), so it reads
//! as the same instrument as the app's Audio Setup scope.
//!
//! Also prints a per-feature jitter index (mean |Δ| per hop, raw vs smoothed)
//! so successive analysis iterations can be compared with a number as well as
//! by eye.
//!
//! `--csv <dir>` additionally writes one `<dir>/<label>.csv` per analyzed clip
//! (label = selftest scenario name, or the input path for file jobs), one row
//! per hop, columns:
//! ```text
//! hop_index,time_s,ground_truth_f0_hz,salience_f0_hz,
//!   full_amplitude,full_brightness,full_noisiness,full_liveliness,full_transients,
//!   low_amplitude,low_brightness,low_noisiness,low_liveliness,low_transients,
//!   mid_amplitude,mid_brightness,mid_noisiness,mid_liveliness,mid_transients,
//!   high_amplitude,high_brightness,high_noisiness,high_liveliness,high_transients,
//!   tracked_f0_hz,
//!   full_pitch,full_presence,low_pitch,low_presence,
//!   mid_pitch,mid_presence,high_pitch,high_presence
//! ```
//! Feature values are the RAW per-hop values (`HopRecord::raw`), not the
//! shaped/smoothed follower output — the CSV is the ground-truth-comparison
//! surface for tracker/salience work, not a performer-feel preview (that's
//! what the PNG's bright trace is for). `ground_truth_f0_hz` is each
//! scenario's own known f0 curve (dive = the exponential glide formula,
//! wobble/growl = constant 150.0) or `NaN` where there is no single tracked
//! fundamental (kicks, busymix, riser) or for file inputs (unknown ground
//! truth). `salience_f0_hz` is the P1 harmonic-sum salience peak (Full window
//! only, `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` D1) converted to Hz, or `NaN`
//! on a fully-floored column. `tracked_f0_hz` is the P2 D5 Full-tracker's
//! `pitch_hz`, `NaN` until the Full tracker has acquired at least once
//! (`pitch_confidence` still exactly 0). `<band>_pitch`/`<band>_presence` are
//! the D5 tracker's per-band `BandFeatures::pitch`/`presence`, RAW (unshaped)
//! per hop, same as the other per-band columns.

use manifold_audio::analysis::{StreamingSendAnalyzer, salience_into, salience_peak, tilt_weights};
use manifold_core::audio_mod::AudioModShape;
use manifold_core::audio_setup::{DEFAULT_LOW_HZ, DEFAULT_MID_HZ, FLOOR_DB_OFF};
use manifold_spectral::SpectrogramConfig;

const BAND_NAMES: [&str; 4] = ["FULL", "LOW", "MID", "HIGH"];
/// Band identity colors, matched to the scope shader's centroid traces
/// (`spectrogram.wgsl` `centroid_line` call sites) so the harness reads with
/// the same legend as the app: Full = magenta, Low = red, Mid = green,
/// High = blue.
const BAND_COLORS: [[u8; 3]; 4] = [
    [255, 64, 217],  // Full — magenta (1.0, 0.25, 0.85)
    [255, 115, 77],  // Low — red (1.0, 0.45, 0.30)
    [102, 255, 128], // Mid — green (0.40, 1.0, 0.50)
    [115, 184, 255], // High — blue (0.45, 0.72, 1.0)
];
const FEATURE_NAMES: [&str; 7] =
    ["AMPLITUDE", "BRIGHTNESS", "NOISINESS", "LIVELINESS", "TRANSIENTS", "PITCH", "PRESENCE"];
/// Index of `PITCH`/`PRESENCE` in [`FEATURE_NAMES`] — the P2 D5 tracker
/// outputs, appended below the original five (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P2).
const PITCH_IDX: usize = 5;
const PRESENCE_IDX: usize = 6;
/// Index of `TRANSIENTS` in [`FEATURE_NAMES`] — the P3 fire-count instrument
/// reads this directly (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P3).
const TRANSIENTS_IDX: usize = 4;
/// The doc's display rule (D6): the PITCH lane only draws where the SAME
/// band's presence has cleared this bar — a low-confidence pitch reading is a
/// held/stale position, not a real one. PRESENCE itself always draws.
const PITCH_DISPLAY_PRESENCE: f32 = 0.25;

/// One hop's worth of everything we plot.
struct HopRecord {
    /// Raw spectrogram column (untilted magnitudes), `num_bins` long.
    col: Vec<f32>,
    /// Per-band centroid height-from-bottom 0..1, -1 = hidden. [Full, Low, Mid, High].
    centroid_yfb: [f32; 4],
    /// Per-band onset fire flags (1.0 on the fired hop). [Low, Mid, High].
    onset_fired: [f32; 3],
    /// features[feature][band], feature order per FEATURE_NAMES, band order per BAND_NAMES.
    raw: [[f32; 4]; 7],
    /// Same, after the default AudioModShape follower (what a param would receive).
    smoothed: [[f32; 4]; 7],
    /// P1 salience peak (Full window only), D1 harmonic-sum over the tilted,
    /// floored column: `fmin · 2^(refined_bin / bpo)`. `NaN` when the column
    /// is fully floored (no peak) — see `manifold_audio::analysis::salience_peak`.
    salience_f0_hz: f32,
    /// P2 D5 Full-tracker `pitch_hz`, `NaN` until the Full tracker has
    /// acquired at least once this clip (`pitch_confidence` still exactly 0 —
    /// see the module doc's CSV column note).
    tracked_f0_hz: f32,
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
    csv_dir: Option<String>,
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
        csv_dir: None,
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
            "--csv" => args.csv_dir = Some(next(&mut i)?),
            s if s.starts_with("--") => return Err(format!("unknown flag {s}")),
            s => args.input = Some(s.to_string()),
        }
        i += 1;
    }
    if !args.selftest && args.input.is_none() {
        return Err("usage: mod_harness <clip> [--out out.png] [--low hz] [--mid hz] [--floor db] [--start s] [--dur s] [--csv dir] | --selftest [--csv dir]".into());
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

    // ── Source: decode file (downmix all channels, same mean the live path
    //    uses) or synthesize the self-test scenarios. One PNG per job.
    let jobs: Vec<(String, Vec<f32>, u32, String, GroundTruthFn)> = if args.selftest {
        synth_selftests()
            .into_iter()
            .map(|(name, mono, gt)| {
                let out = args
                    .out
                    .strip_suffix(".png")
                    .map(|stem| format!("{stem}_{name}.png"))
                    .unwrap_or_else(|| format!("{}_{name}.png", args.out));
                (name.to_string(), mono, SELFTEST_SR, out, gt)
            })
            .collect()
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
        // Optional excerpt (file input only).
        let sr = decoded.sample_rate;
        let start = ((args.start_s.max(0.0) * sr as f32) as usize).min(mono.len());
        let len = if args.dur_s.is_finite() {
            ((args.dur_s.max(0.0) * sr as f32) as usize).min(mono.len() - start)
        } else {
            mono.len() - start
        };
        if len == 0 {
            eprintln!("empty excerpt (start/dur out of range)");
            std::process::exit(1);
        }
        // File inputs carry no known ground truth.
        vec![(path, mono[start..start + len].to_vec(), sr, args.out.clone(), gt_none as GroundTruthFn)]
    };

    for (label, mono, sr, out, gt) in &jobs {
        analyze_and_render(label, mono, *sr, out, &args, *gt);
    }
}

/// Run the live analysis over one mono clip and write its PNG + jitter report
/// (+ CSV, if `--csv` was passed). `ground_truth(time_s)` is the scenario's
/// own known f0 curve, or `gt_none` (NaN) when there is no single tracked
/// fundamental / the input is a file.
fn analyze_and_render(
    label: &str,
    mono: &[f32],
    sr: u32,
    out: &str,
    args: &Args,
    ground_truth: GroundTruthFn,
) {
    // ── Analysis: the exact live path, fed causally one hop at a time so
    //    `latest()` is sampled at hop rate — what the modulation evaluator sees.
    let cfg = SpectrogramConfig::default();
    let hop = cfg.hop.max(1);
    let dt = hop as f32 / sr as f32;
    let mut an = StreamingSendAnalyzer::new(sr, args.low_hz, args.mid_hz);
    an.set_floor_db(args.floor_db);
    an.set_scope(true);
    // P2: the harness always runs with the D5 tracker on (D7's runtime
    // activation OR-gate is app-side, P4's job — the harness is the eval
    // loop, not a project, so it always exercises the tracker).
    an.set_pitch_tracking(true);
    let num_bins = an.num_bins();
    let (fmin, fmax) = an.freq_range();
    let bpo = cfg.bpo;
    // Tilt weights are kept ONLY for the naive "before" baseline print — the
    // display column. Salience itself reads the UNTILTED floored column (D1 as
    // amended 2026-07-06): the +3dB/oct tilt hands the self-similar sub-comb
    // at 4x/8x f0 exactly the boost it needs to out-salience the fundamental
    // (measured on the dive: 22.3% tilted -> 66.4% untilted per-hop hit rate).
    let tilt_w = tilt_weights(&cfg, sr as f32, num_bins);
    let mut salience_scratch = vec![0.0f32; num_bins];

    let shape = AudioModShape::default();
    let mut smooth_state = [[0.0f32; 4]; 7];
    let mut prev_raw = [[0.0f32; 4]; 7];
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
            let mut raw = [[0.0f32; 4]; 7];
            let mut smoothed = [[0.0f32; 4]; 7];
            for b in 0..4 {
                let bf = &f.bands[b];
                let vals = [
                    bf.amplitude,
                    bf.brightness,
                    bf.noisiness,
                    bf.liveliness,
                    bf.transients,
                    bf.pitch,
                    bf.presence,
                ];
                for (fi, &v) in vals.iter().enumerate() {
                    raw[fi][b] = v;
                    smoothed[fi][b] =
                        shape.apply(v, dt, &mut smooth_state[fi][b], &mut prev_raw[fi][b]);
                }
            }
            // Salience reads the UNTILTED floored column (D1 as amended
            // 2026-07-06 — the raw scope column is already floored, so this is
            // exactly the analyzer's `vqt_raw` post-floor).
            salience_into(&col, bpo, &mut salience_scratch);
            let salience_f0_hz = match salience_peak(&salience_scratch) {
                Some((bin, _peak_val)) => fmin * 2f32.powf(bin / bpo as f32),
                None => f32::NAN,
            };
            // P2: the Full tracker's Hz reading, NaN until it has acquired at
            // least once this clip (see the module doc's CSV column note).
            let tracked_f0_hz = if f.pitch_confidence > 0.0 { f.pitch_hz } else { f32::NAN };
            records.push(HopRecord {
                col,
                centroid_yfb: centroid,
                onset_fired: onsets,
                raw,
                smoothed,
                salience_f0_hz,
                tracked_f0_hz,
            });
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

    // ── P1 report: naive argmax (no harmonic sum) vs salience argmax,
    //    percentage of hops (after warm-up) within ±2 bins of the scenario's
    //    own known f0 — the numeric gate from
    //    docs/AUDIO_OBJECT_TRACKING_DESIGN.md P1. dive/growl only: they carry
    //    a real ground-truth curve; wobble/kicks/busymix/riser and file jobs
    //    don't (no gate at P1 — see the doc's P1 section).
    if args.selftest && (label == "dive" || label == "growl") {
        // P1 gate, as amended 2026-07-06 (see AUDIO_OBJECT_TRACKING_DESIGN P1):
        // per-hop argmax is "trackable", not near-perfect — the dive's 7-voice
        // beating genuinely cancels the fundamental for ~4-hop stretches, which
        // no memoryless estimator can beat; the D5 tracker's hold (38 hops)
        // absorbs it. Gate: dive >= 60% AND max miss-run <= 38 hops; growl
        // (no beating) >= 95%. The >= 95% smoothness bar lives in P2's tracked
        // trajectory, where temporal integration exists.
        const WARMUP_HOPS: usize = 32;
        let bpo_f = bpo as f32;
        let gt_bin = |f0_hz: f32| bpo_f * (f0_hz / fmin).max(1e-6).log2();
        let (mut naive_hits, mut sal_hits, mut total) = (0usize, 0usize, 0usize);
        let (mut miss_run, mut max_miss_run) = (0usize, 0usize);
        for (idx, r) in records.iter().enumerate().skip(WARMUP_HOPS) {
            let f0 = ground_truth(idx as f32 * dt);
            if !f0.is_finite() {
                continue;
            }
            let want = gt_bin(f0);
            total += 1;
            let tilted: Vec<f32> = r.col.iter().zip(tilt_w.iter()).map(|(&c, &w)| c * w).collect();
            if let Some(nb) = naive_argmax_bin(&tilted)
                && (nb - want).abs() <= 2.0
            {
                naive_hits += 1;
            }
            let hit = r.salience_f0_hz.is_finite() && (gt_bin(r.salience_f0_hz) - want).abs() <= 2.0;
            if hit {
                sal_hits += 1;
                miss_run = 0;
            } else {
                miss_run += 1;
                max_miss_run = max_miss_run.max(miss_run);
            }
        }
        let naive_pct = 100.0 * naive_hits as f64 / total.max(1) as f64;
        let sal_pct = 100.0 * sal_hits as f64 / total.max(1) as f64;
        let gate_pct = if label == "dive" { 60.0 } else { 95.0 };
        println!(
            "{label}: naive {naive_pct:.1}% -> salience {sal_pct:.1}% (gate >= {gate_pct}%), max miss-run {max_miss_run} hops (gate <= 38)"
        );
    }

    // ── P2 gates (docs/AUDIO_OBJECT_TRACKING_DESIGN.md P2): the D5 tracker's
    // numeric acceptance bar, one line per metric, selftest only.
    if args.selftest {
        print_p3_fires(label, &records);
        print_p2_gates(label, &records, dt, ground_truth);
    }

    if let Some(dir) = &args.csv_dir {
        write_csv(dir, label, &records, dt, ground_truth);
    }

    render_png(out, &records, &cfg, sr, args.low_hz, args.mid_hz);
    println!("wrote {out}");
}

/// Naive argmax (no D1 harmonic sum): the loudest bin of a tilted column as-
/// is, the P1 report's "before" baseline. `None` on a fully-floored column
/// (mirrors [`salience_peak`]'s convention).
fn naive_argmax_bin(tilted: &[f32]) -> Option<f32> {
    let (mut bk, mut bv) = (0usize, *tilted.first()?);
    for (k, &v) in tilted.iter().enumerate().skip(1) {
        if v > bv {
            bk = k;
            bv = v;
        }
    }
    (bv > 0.0).then_some(bk as f32)
}

/// `f_hz` in semitones relative to `ref_hz` (signed, `12 * log2(f/ref)`).
fn semitones_vs(f_hz: f32, ref_hz: f32) -> f32 {
    12.0 * (f_hz / ref_hz.max(1e-6)).log2()
}

/// P3 sweep instrument (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P3, BUG-041):
/// counts hops (after warm-up) where a band's `Transients` feature hit its
/// fired value (`> 0.999` — the same threshold `update_trackers` reads to
/// treat a hop as "this band's onset fired this hop", not the decayed tail).
/// Only Full and Low are counted — the two bands the P3 gates and the dive/
/// riser/growl/kicks/busymix scenarios care about. Wobble prints its count
/// with no gate attached (its LFO re-attacks are arguably genuine onsets;
/// its real gate is the P2 `pitch_stddev_st` line). This is a raw count, not
/// a PASS/FAIL judgment — the sweep script reads the numbers against the
/// gates named in the printed reminder.
///
/// `WARMUP_HOPS` here is deliberately SMALLER than the P1/P2 reports' 32 (a
/// pitch-tracker fade-in guard, unrelated to onsets): every selftest scenario
/// throws exactly one unavoidable cold-start fire around hop 17-19, the
/// structural artifact of the first real column being compared against the
/// zero-seeded `prev_col` before any real predecessor exists — not part of
/// BUG-041's continuous false-firing. 20 clears that single artifact while
/// still counting `kicks`' own first kick (hop ~21, a real onset the P3
/// "exactly 8" gate requires) — verified against the sweep CSVs.
fn print_p3_fires(label: &str, records: &[HopRecord]) {
    const WARMUP_HOPS: usize = 20;
    const FULL: usize = 0;
    const LOW: usize = 1;
    let post = &records[WARMUP_HOPS.min(records.len())..];
    let full_fires = post.iter().filter(|r| r.raw[TRANSIENTS_IDX][FULL] > 0.999).count();
    let low_fires = post.iter().filter(|r| r.raw[TRANSIENTS_IDX][LOW] > 0.999).count();
    println!(
        "P3 {label}: full_fires={full_fires} low_fires={low_fires} (gates: dive full 0, kicks low == 8, busymix low >= 7, riser full 0, growl full 0)"
    );
}

/// P2 numeric gates (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P2's phase-report
/// bar), one `P2 <scenario>: <metric>=<value> (gate <op> <bound>) PASS|FAIL`
/// line per metric, per selftest scenario. Reads only `HopRecord::raw`
/// (RAW, unshaped values — the ground-truth-comparison surface, same
/// convention as the CSV) and `tracked_f0_hz`.
fn print_p2_gates(label: &str, records: &[HopRecord], dt: f32, ground_truth: GroundTruthFn) {
    // Same warm-up skip as the P1 report — the zero-padded fade-in never
    // carries a real reading, on either side of the comparison.
    const WARMUP_HOPS: usize = 32;
    // Presence threshold for counting a DISTINCT acquisition event (an
    // interior harness choice, not part of D5 itself — the tracker's own
    // presence never returns to *exactly* 0 after a real dropout within a
    // few-second clip, so a small-but-decisive bar is what makes "how many
    // times did this reacquire" countable at all).
    const ACQUIRE_PRESENCE: f32 = 0.02;
    const FULL: usize = 0;
    const LOW: usize = 1;

    let gate = |metric: &str, value: f64, op: &str, bound: f64, pass: bool| {
        println!("P2 {label}: {metric}={value:.4} (gate {op} {bound}) {}", if pass { "PASS" } else { "FAIL" });
    };

    match label {
        "dive" => {
            let acquired: Vec<&HopRecord> = records.iter().skip(WARMUP_HOPS).filter(|r| r.tracked_f0_hz.is_finite()).collect();
            if acquired.len() < 2 {
                gate("max_delta_st", f64::NAN, "<=", 1.0, false);
                gate("mean_delta_st_per_hop", f64::NAN, "<=", 0.15, false);
                gate("pct_within_1st_of_gt", 0.0, ">=", 95.0, false);
                return;
            }
            let (mut max_delta, mut sum_delta) = (0.0f32, 0.0f64);
            for w in acquired.windows(2) {
                let d = semitones_vs(w[1].tracked_f0_hz, w[0].tracked_f0_hz).abs();
                max_delta = max_delta.max(d);
                sum_delta += d as f64;
            }
            let mean_delta = sum_delta / (acquired.len() - 1) as f64;

            // Denominator is every hop with a KNOWN ground truth, not just the
            // acquired subset — a hop the tracker never reached is a miss,
            // not an exclusion.
            let (mut total_gt, mut within) = (0usize, 0usize);
            for (idx, r) in records.iter().enumerate().skip(WARMUP_HOPS) {
                let gt = ground_truth(idx as f32 * dt);
                if !gt.is_finite() {
                    continue;
                }
                total_gt += 1;
                if r.tracked_f0_hz.is_finite() && semitones_vs(r.tracked_f0_hz, gt).abs() <= 1.0 {
                    within += 1;
                }
            }
            let within_pct = 100.0 * within as f64 / total_gt.max(1) as f64;

            gate("max_delta_st", max_delta as f64, "<=", 1.0, max_delta <= 1.0);
            gate("mean_delta_st_per_hop", mean_delta, "<=", 0.15, mean_delta <= 0.15);
            gate("pct_within_1st_of_gt", within_pct, ">=", 95.0, within_pct >= 95.0);
        }
        "wobble" | "growl" => {
            let semis: Vec<f64> = records
                .iter()
                .skip(WARMUP_HOPS)
                .filter(|r| r.tracked_f0_hz.is_finite())
                .map(|r| semitones_vs(r.tracked_f0_hz, 150.0) as f64)
                .collect();
            if semis.is_empty() {
                gate("pitch_stddev_st", f64::NAN, "<=", 0.5, false);
                return;
            }
            let mean = semis.iter().sum::<f64>() / semis.len() as f64;
            let var = semis.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / semis.len() as f64;
            let stddev = var.sqrt();
            gate("pitch_stddev_st", stddev, "<=", 0.5, stddev <= 0.5);
        }
        "riser" => {
            let post = &records[WARMUP_HOPS.min(records.len())..];
            let quiet = post.iter().filter(|r| r.raw[PRESENCE_IDX][FULL] <= 0.15).count();
            let quiet_pct = 100.0 * quiet as f64 / post.len().max(1) as f64;

            let mut acquisitions = 0usize;
            let mut was_acquired = false;
            for r in post {
                let acquired = r.raw[PRESENCE_IDX][FULL] >= ACQUIRE_PRESENCE;
                if acquired && !was_acquired {
                    acquisitions += 1;
                }
                was_acquired = acquired;
            }

            gate("pct_hops_full_presence_le_0.15", quiet_pct, ">=", 90.0, quiet_pct >= 90.0);
            gate("distinct_full_acquisitions", acquisitions as f64, "<=", 2.0, acquisitions <= 2);
        }
        "kicks" => {
            let post = &records[WARMUP_HOPS.min(records.len())..];
            let hot = post.iter().filter(|r| r.raw[PRESENCE_IDX][LOW] > 0.5).count();
            let hot_pct = 100.0 * hot as f64 / post.len().max(1) as f64;
            gate("pct_hops_low_presence_gt_0.5", hot_pct, "<=", 20.0, hot_pct <= 20.0);
        }
        _ => {}
    }
}

/// One `<dir>/<label>.csv`, one row per hop: hop index, time, ground-truth
/// f0 (or NaN), the P1 salience-peak f0 estimate (or NaN), then the five raw
/// features for each of the four bands — see the module header comment for
/// the exact column layout this must match.
fn write_csv(dir: &str, label: &str, records: &[HopRecord], dt: f32, ground_truth: GroundTruthFn) {
    std::fs::create_dir_all(dir).unwrap_or_else(|e| {
        eprintln!("failed to create csv dir {dir}: {e}");
        std::process::exit(1);
    });
    let path = format!("{dir}/{label}.csv");
    let mut csv = String::from("hop_index,time_s,ground_truth_f0_hz,salience_f0_hz");
    for band in BAND_NAMES {
        let band_lc = band.to_ascii_lowercase();
        for feat in &FEATURE_NAMES[..5] {
            csv.push_str(&format!(",{band_lc}_{}", feat.to_ascii_lowercase()));
        }
    }
    csv.push_str(",tracked_f0_hz");
    for band in BAND_NAMES {
        let band_lc = band.to_ascii_lowercase();
        csv.push_str(&format!(",{band_lc}_pitch,{band_lc}_presence"));
    }
    csv.push('\n');
    for (idx, r) in records.iter().enumerate() {
        let t = idx as f32 * dt;
        csv.push_str(&format!("{idx},{t:.6},{},{}", ground_truth(t), r.salience_f0_hz));
        for b in 0..4 {
            for fi in 0..5 {
                csv.push_str(&format!(",{}", r.raw[fi][b]));
            }
        }
        csv.push_str(&format!(",{}", r.tracked_f0_hz));
        for b in 0..4 {
            csv.push_str(&format!(",{},{}", r.raw[PITCH_IDX][b], r.raw[PRESENCE_IDX][b]));
        }
        csv.push('\n');
    }
    std::fs::write(&path, &csv).unwrap_or_else(|e| {
        eprintln!("failed to write {path}: {e}");
        std::process::exit(1);
    });
    println!("wrote {path}");
}

// ── Self-test signals ────────────────────────────────────────────────────

const SELFTEST_SR: u32 = 48_000;
const SELFTEST_SECS: usize = 4;

/// A scenario's own known f0 curve, sampled at a hop's time-since-start
/// (seconds). `f32::NAN` where there is no single tracked fundamental (or,
/// for file jobs, where ground truth is simply unknown) — see `gt_none`.
type GroundTruthFn = fn(f32) -> f32;

/// Six isolated scenarios, one PNG each — what each picture must show:
/// `dive` — supersaw glide 1200→150 Hz; the centroid trace follows it down.
/// `wobble` — 150 Hz bass, 3 Hz amplitude LFO; the amplitude lane oscillates.
/// `kicks` — kick every 0.5 s on silence; transients tick at exactly 2 Hz.
/// `busymix` — saw + noise pad + kicks; the stress case where features fight.
/// `riser` — band-limited noise whose center sweeps 200 Hz→8 kHz, no tonal
/// content; the presence-null case (no stable harmonic peak at any hop).
/// `growl` — 150 Hz saw at CONSTANT pitch with a 2 Hz spectral-tilt wobble;
/// the constant-pitch-moving-timbre case (approximated formant motion).
fn synth_selftests() -> Vec<(&'static str, Vec<f32>, GroundTruthFn)> {
    vec![
        ("dive", soft_clip(synth_dive()), gt_dive),
        ("wobble", soft_clip(synth_wobble()), gt_const_150),
        ("kicks", soft_clip(synth_kicks(Vec::new())), gt_none),
        ("busymix", soft_clip(synth_kicks(synth_busy_pad())), gt_none),
        ("riser", soft_clip(synth_riser()), gt_none),
        ("growl", soft_clip(synth_growl()), gt_const_150),
    ]
}

/// Ground truth for `dive`: the same exponential glide formula `synth_dive`
/// synthesizes from, evaluated at time-since-start.
fn gt_dive(t: f32) -> f32 {
    let secs = SELFTEST_SECS as f32;
    1200.0 * (150.0f32 / 1200.0).powf(t / secs)
}

/// Ground truth for `wobble`/`growl`: fixed 150 Hz fundamental throughout.
fn gt_const_150(_t: f32) -> f32 {
    150.0
}

/// Ground truth for scenarios with no single tracked fundamental
/// (kicks/busymix/riser) and for file inputs (unknown ground truth).
fn gt_none(_t: f32) -> f32 {
    f32::NAN
}

fn selftest_buf() -> Vec<f32> {
    vec![0.0f32; SELFTEST_SECS * SELFTEST_SR as usize]
}

/// Supersaw: 7 naive saws detuned ±12 cents (aliasing is irrelevant for eval),
/// gliding exponentially 1200→150 Hz over the clip.
fn synth_dive() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let secs = SELFTEST_SECS as f32;
    let detunes: [f32; 7] = [-0.12, -0.08, -0.04, 0.0, 0.04, 0.08, 0.12];
    let mut phases = [0.0f32; 7];
    for (i, s_out) in out.iter_mut().enumerate() {
        let t = i as f32 / srf;
        let f0 = 1200.0 * (150.0f32 / 1200.0).powf(t / secs);
        let mut s = 0.0;
        for (p, det) in phases.iter_mut().zip(detunes) {
            let f = f0 * 2.0f32.powf(det / 12.0);
            *p = (*p + f / srf).fract();
            s += 2.0 * *p - 1.0;
        }
        *s_out += 0.12 * s;
    }
    out
}

/// Wobble bass: 150 Hz saw, amplitude LFO at 3 Hz.
fn synth_wobble() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let mut p = 0.0f32;
    for (i, s_out) in out.iter_mut().enumerate() {
        let t = i as f32 / srf;
        p = (p + 150.0 / srf).fract();
        let lfo = 0.5 + 0.5 * (std::f32::consts::TAU * 3.0 * t).sin();
        *s_out += 0.5 * lfo * (2.0 * p - 1.0);
    }
    out
}

/// Saw + white-ish noise pad (LCG noise) across the whole clip.
fn synth_busy_pad() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let mut seed = 0x2545F491u32;
    let mut p = 0.0f32;
    for s_out in out.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
        p = (p + 220.0 / srf).fract();
        *s_out += 0.25 * (2.0 * p - 1.0) + 0.15 * noise;
    }
    out
}

/// Kick every 0.5 s — a 90 ms pitch-swept sine 120→45 Hz with exp decay —
/// added onto `base` (empty ⇒ kicks on silence).
fn synth_kicks(base: Vec<f32>) -> Vec<f32> {
    let mut out = if base.is_empty() { selftest_buf() } else { base };
    let n = out.len();
    let srf = SELFTEST_SR as f32;
    for k in 0..(2 * SELFTEST_SECS) {
        let start = k * SELFTEST_SR as usize / 2;
        let mut ph = 0.0f32;
        for j in 0..(SELFTEST_SR as usize * 9 / 100) {
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
    out
}

/// Filtered-noise riser: white LCG noise through a swept two-one-pole
/// bandpass (high-pass then low-pass, both tracking a moving center that
/// sweeps 200 Hz→8 kHz exponentially over the clip) — no tonal content at
/// any hop, so no salience peak should ever look stable. Amplitude grows
/// slightly over the clip, the way a riser builds.
fn synth_riser() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let secs = SELFTEST_SECS as f32;
    let nyquist = srf * 0.45;
    let mut seed = 0xACE1_u32;
    let mut hp_lp_state = 0.0f32; // internal lowpass whose complement is the highpass
    let mut bp_state = 0.0f32; // final lowpass stage, yields the bandpass output
    for (i, s_out) in out.iter_mut().enumerate() {
        let t = i as f32 / srf;
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
        let center = 200.0 * (8000.0f32 / 200.0).powf(t / secs);
        let fc_hp = (center * 0.6).min(nyquist);
        let fc_lp = (center * 1.6).min(nyquist);
        let a_hp = 1.0 - (-std::f32::consts::TAU * fc_hp / srf).exp();
        hp_lp_state += a_hp * (noise - hp_lp_state);
        let high_passed = noise - hp_lp_state;
        let a_lp = 1.0 - (-std::f32::consts::TAU * fc_lp / srf).exp();
        bp_state += a_lp * (high_passed - bp_state);
        let amp = 0.25 + 0.15 * (t / secs);
        *s_out += amp * bp_state;
    }
    out
}

/// Growl: 150 Hz saw held at CONSTANT pitch, mixed with a one-pole low-passed
/// copy whose cutoff oscillates at 2 Hz between ~300 Hz and ~3 kHz (moving
/// spectral tilt = brightness/energy motion, approximating formant motion),
/// plus a mild 2 Hz amplitude wobble (depth ~0.3). The fundamental never
/// moves — only the timbre does.
fn synth_growl() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let mut p = 0.0f32;
    let mut lp_state = 0.0f32;
    for (i, s_out) in out.iter_mut().enumerate() {
        let t = i as f32 / srf;
        p = (p + 150.0 / srf).fract();
        let saw = 2.0 * p - 1.0;
        let tilt_lfo = 0.5 + 0.5 * (std::f32::consts::TAU * 2.0 * t).sin();
        let fc = 300.0 + (3000.0 - 300.0) * tilt_lfo;
        let a = 1.0 - (-std::f32::consts::TAU * fc / srf).exp();
        lp_state += a * (saw - lp_state);
        let mixed = 0.5 * saw + 0.5 * lp_state;
        let amp = 1.0 + 0.3 * (std::f32::consts::TAU * 2.0 * t).sin();
        *s_out += 0.5 * amp * mixed;
    }
    out
}

fn soft_clip(mut v: Vec<f32>) -> Vec<f32> {
    for s in &mut v {
        *s = s.tanh();
    }
    v
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

    // ── Spectrogram: the scope shader's exact display transform, ported from
    //    `spectrogram.wgsl` fs_main — 2-tap blend between adjacent log bins in
    //    the POWER domain (smooth, not blocky), dB, pink tilt in display-y form,
    //    fixed db_min..db_max window, jet ramp. Bin 0 at the bottom. The only
    //    departure: each pixel column takes the bucket MAX per bin when hops
    //    are decimated, so peaks stay visible.
    let tilt_range = (cfg.effective_fmax(sr as f32) / cfg.fmin.max(1.0)).log2();
    for x in 0..w {
        let (lo, hi) = bucket(x);
        for py in 0..SPEC_H {
            let uv_y = py as f32 / (SPEC_H - 1) as f32; // 0 top → 1 bottom
            let log_bin_f = ((1.0 - uv_y) * (num_bins - 1) as f32).clamp(0.0, (num_bins - 1) as f32);
            let b_lo = log_bin_f as usize;
            let b_hi = (b_lo + 1).min(num_bins - 1);
            let frac = log_bin_f - b_lo as f32;
            let (mut mag_lo, mut mag_hi) = (0.0f32, 0.0f32);
            for r in &records[lo..hi] {
                mag_lo = mag_lo.max(r.col[b_lo]);
                mag_hi = mag_hi.max(r.col[b_hi]);
            }
            let power = mag_lo * mag_lo + (mag_hi * mag_hi - mag_lo * mag_lo) * frac;
            let db = 10.0 * (power + 1e-18).log10();
            let tilt = cfg.tilt_slope * tilt_range * (0.5 - uv_y);
            let norm = ((db + tilt - cfg.db_min) / (cfg.db_max - cfg.db_min)).clamp(0.0, 1.0);
            img.put_pixel((x0 + x) as u32, (y + py) as u32, image::Rgb(colormap(norm)));
        }
    }
    // Band divider lines at the same geometric mapping the analysis uses.
    let bin_of = |hz: f32| -> usize {
        ((cfg.bpo as f32 * (hz / cfg.fmin.max(1.0)).max(1e-6).log2()).round() as i64)
            .clamp(1, num_bins as i64 - 1) as usize
    };
    let inv_nb = if num_bins > 1 { 1.0 / (num_bins - 1) as f32 } else { 0.0 };
    for hz in [low_hz, mid_hz] {
        let py = SPEC_H - 1 - (bin_of(hz) as f32 * inv_nb * (SPEC_H - 1) as f32) as usize;
        for x in 0..w {
            // Divider color/alpha from the shader's `divider()` (line, unhovered).
            blend_pixel(&mut img, x0 + x, y + py, [224, 224, 237], 0.6);
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
        // P1 salience-peak dot (Full window only, D1 harmonic-sum fundamental):
        // small bright marker riding the fundamental, same global-display-y
        // mapping as the centroid traces above. Only drawn where a peak
        // exists (fully-floored hops carry NaN — nothing to draw).
        if r.salience_f0_hz.is_finite() {
            let sal_bin = cfg.bpo as f32 * (r.salience_f0_hz / cfg.fmin.max(1.0)).max(1e-6).log2();
            let sal_yfb = (sal_bin * inv_nb).clamp(0.0, 1.0);
            let py = SPEC_H - 1 - (sal_yfb * (SPEC_H - 1) as f32) as usize;
            for dy in 0..2usize {
                for dx in 0..2usize {
                    blend_pixel(&mut img, x0 + x + dx, y + py.saturating_sub(dy), [255, 250, 235], 0.95);
                }
            }
        }
        // Transient ticks: three stacked lanes at the BOTTOM edge, Low lowest —
        // same layout, colors, and alpha as the shader's onset lanes.
        const TICK_COLORS: [[u8; 3]; 3] = [
            [255, 89, 77],   // Low (1.0, 0.35, 0.30)
            [89, 255, 115],  // Mid (0.35, 1.0, 0.45)
            [102, 158, 255], // High (0.40, 0.62, 1.0)
        ];
        let lane_px = (SPEC_H as f32 * 0.014) as usize;
        for (oi, &tick_color) in TICK_COLORS.iter().enumerate() {
            if records[lo..hi].iter().any(|rec| rec.onset_fired[oi] > 0.5) {
                let lane_bottom = SPEC_H - oi * lane_px;
                for py in (lane_bottom - lane_px)..lane_bottom {
                    blend_pixel(&mut img, x0 + x, y + py, tick_color, 0.85);
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
                // D6 display rule: the PITCH lane draws a band's trace only
                // where that SAME band's presence has cleared the bar — a
                // low-confidence pitch reading is a held/stale position, not
                // a real one. PRESENCE (and every other lane) always draws.
                let visible = |r: &HopRecord| fi != PITCH_IDX || r.raw[PRESENCE_IDX][b] >= PITCH_DISPLAY_PRESENCE;

                // Raw: min..max span, dim — the honest jitter.
                let (mut mn, mut mx) = (f32::MAX, f32::MIN);
                let mut any = false;
                for r in &records[lo..hi] {
                    if !visible(r) {
                        continue;
                    }
                    any = true;
                    mn = mn.min(r.raw[fi][b]);
                    mx = mx.max(r.raw[fi][b]);
                }
                if !any {
                    continue;
                }
                let py_of = |v: f32| LANE_H - 1 - ((v.clamp(0.0, 1.0)) * (LANE_H - 1) as f32) as usize;
                for py in py_of(mx)..=py_of(mn) {
                    blend_pixel(&mut img, x0 + x, y + py, color, 0.22);
                }
                // Smoothed: single bright trace (bucket mean over the visible
                // hops only).
                let (mut sm, mut cnt) = (0.0f32, 0usize);
                for r in &records[lo..hi] {
                    if !visible(r) {
                        continue;
                    }
                    sm += r.smoothed[fi][b];
                    cnt += 1;
                }
                let sm = sm / cnt.max(1) as f32;
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

/// The scope's jet ramp, ported LINE-FOR-LINE from `spectrogram.wgsl`
/// `colormap()` (manifold-spectral) — black → navy → blue → cyan → green →
/// yellow → red → white, same non-uniform stops — so the harness spectrogram
/// is pixel-comparable with the app's. If the shader's ramp is ever retuned,
/// retune this copy with it (the shader carries the matching cross-reference).
fn colormap(t_in: f32) -> [u8; 3] {
    const STOPS: [(f32, f32, [f32; 3], [f32; 3]); 7] = [
        (0.00, 0.15, [0.00, 0.00, 0.00], [0.00, 0.00, 0.45]), // black → deep navy
        (0.15, 0.35, [0.00, 0.00, 0.45], [0.00, 0.10, 0.95]), // → blue
        (0.35, 0.55, [0.00, 0.10, 0.95], [0.00, 0.80, 0.95]), // → cyan
        (0.55, 0.70, [0.00, 0.80, 0.95], [0.20, 0.95, 0.20]), // → green
        (0.70, 0.80, [0.20, 0.95, 0.20], [0.95, 0.95, 0.00]), // → yellow
        (0.80, 0.90, [0.95, 0.95, 0.00], [0.95, 0.00, 0.00]), // → red
        (0.90, 1.00, [0.95, 0.00, 0.00], [1.00, 1.00, 1.00]), // → white (peaks)
    ];
    let t = t_in.clamp(0.0, 1.0);
    for &(t0, t1, c0, c1) in &STOPS {
        if t < t1 || t1 >= 1.0 {
            let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            return [
                (255.0 * (c0[0] + (c1[0] - c0[0]) * f)) as u8,
                (255.0 * (c0[1] + (c1[1] - c0[1]) * f)) as u8,
                (255.0 * (c0[2] + (c1[2] - c0[2]) * f)) as u8,
            ];
        }
    }
    [255, 255, 255]
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
