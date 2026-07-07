//! BUG-046 offline HPSS prototype — P6a of docs/AUDIO_OBJECT_TRACKING_DESIGN.md (D9).
//!
//! PROTOTYPE-ONLY sweep tool, no runtime integration: replicates the live
//! column formation + SuperFlux fire path of `analysis.rs` exactly (validated:
//! with the mask off, per-band fire counts must MATCH `mod_harness` on all 25
//! fixtures — the BUG-044 replica precedent), then sweeps causal
//! harmonic-suppression masks over cached columns and scores each config on:
//!
//!   recovery  — fraction of baseline drums-stem-Low fires that a mix-Low fire
//!               matches within ±35 ms (BUG-046's oracle),
//!   spurious  — mix-Low fires with no baseline drums fire (Low∪Full) within
//!               ±50 ms (mask flutter manufacturing flux would show here),
//!   retention — per-stem Low fire counts vs the mask-off baseline,
//!   guards    — the P3/BUG-044 selftest fire gates replayed on copies of the
//!               mod_harness synth scenarios (dive/riser/growl 0; kicks == 8;
//!               busymix ≥ 7; densemix ≥ 6).
//!
//! Usage:
//!   cargo run --release -p manifold-audio --example hpss_proto            # everything
//!   cargo run --release -p manifold-audio --example hpss_proto -- --validate-only
//!
//! The synth scenario generators are copied verbatim from mod_harness.rs
//! (examples can't share modules without refactoring the harness; this tool
//! dies with P6a, the harness stays authoritative).

use manifold_spectral::SpectrogramConfig;
use std::collections::VecDeque;

// ── Constants copied from analysis.rs (the replica contract: these MUST match
//    the live detector; validation catches drift) ────────────────────────────
const SUPERFLUX_THRESH_FACTOR: f32 = 7.0;
const SUPERFLUX_DELTA: f32 = 48.0;
const SUPERFLUX_NOVELTY_FACTOR: f32 = 2.0;
const SUPERFLUX_NOVELTY_DELTA: f32 = 125.0;
const ODF_NOVELTY_LO: usize = 1;
const ODF_NOVELTY_HI: usize = 10;
const MAXFILTER_RADIUS: usize = 1;
const ODF_MEDIAN_HOPS: usize = 16;
const ODF_PEAK_LOOKBACK: usize = 4;
const ONSET_REFRACTORY_HOPS: u8 = 6;

/// mod_harness's `print_p3_fires` warm-up skip (clears the one structural
/// cold-start fire) — guard counts use the same bar.
const WARMUP_HOPS: usize = 20;

// ── Cached per-clip columns (the exact tilted, floored feature column) ──────

struct Clip {
    num_bins: usize,
    low_bin: usize,
    mid_bin: usize,
    n_hops: usize,
    hop: usize,
    /// n_hops × num_bins, hop-major — the replica of `state.col` per hop.
    cols: Vec<f32>,
}

impl Clip {
    fn col(&self, i: usize) -> &[f32] {
        &self.cols[i * self.num_bins..(i + 1) * self.num_bins]
    }
}

/// Replica of `band_edges` in analysis.rs (private there).
fn band_edges(cfg: &SpectrogramConfig, num_bins: usize, low_hz: f32, mid_hz: f32) -> (usize, usize) {
    let nb = num_bins.max(1);
    let fmin = cfg.fmin.max(1.0);
    let bpo = cfg.bpo as f32;
    let bin_of = |hz: f32| {
        ((bpo * (hz / fmin).max(1e-6).log2()).round() as i64).clamp(1, nb as i64 - 1) as usize
    };
    let low_bin = bin_of(low_hz).min(nb.saturating_sub(2).max(1));
    let mid_bin = bin_of(mid_hz).max(low_bin + 1).min(nb.saturating_sub(1));
    (low_bin, mid_bin)
}

/// Form every hop's tilted, floored column exactly as `StreamingSendAnalyzer::push`
/// does when fed hop-sized chunks (mod_harness's cadence): column i reads the
/// n_fft-sample tail ending at sample (i+1)·hop, zero-padded before the stream
/// fills one window. Floor off ⇒ lin floor at `db_min` (the push-loop default).
fn build_clip(mono: &[f32], sr: u32, low_hz: f32, mid_hz: f32) -> Clip {
    let srf = sr as f32;
    // BUG-052 parity: the live analyzer derives hop/n_fft from the device rate
    // so a hop is always ~5.33 ms; without this the prototype hops 256 samples
    // at NATIVE rate (5.8 ms at 44.1k fixtures) — an 8.8% cadence drift that
    // flips borderline ridge fires and breaks the exact-match gate (surfaced
    // by the 2026-07-07 w10→w6 kick retune; masked at w10).
    let cfg = SpectrogramConfig::default().with_time_grid_for(srf);
    let num_bins = cfg.num_bins(srf).max(1);
    let n_fft = cfg.n_fft;
    let hop = cfg.hop.max(1);
    let mut cqt = cfg.build_transform(srf);
    let tilt = manifold_audio::analysis::tilt_weights(&cfg, srf, num_bins);
    let lin_floor = 10f32.powf(cfg.db_min / 20.0);
    let (low_bin, mid_bin) = band_edges(&cfg, num_bins, low_hz, mid_hz);

    let n_hops = mono.len() / hop;
    let mut cols = vec![0.0f32; n_hops * num_bins];
    let mut vqt_in = vec![0.0f32; n_fft];
    let mut vqt_raw = vec![0.0f32; num_bins];
    for i in 0..n_hops {
        let end = (i + 1) * hop;
        let start = end.saturating_sub(n_fft);
        let seg = &mono[start..end];
        let pad = n_fft - seg.len();
        vqt_in[..pad].fill(0.0);
        vqt_in[pad..].copy_from_slice(seg);
        cqt.process_magnitudes(&vqt_in, &mut vqt_raw);
        let out = &mut cols[i * num_bins..(i + 1) * num_bins];
        for (o, (&r, &w)) in out.iter_mut().zip(vqt_raw.iter().zip(tilt.iter())) {
            let c = r * w;
            *o = if c < lin_floor { 0.0 } else { c };
        }
    }
    Clip { num_bins, low_bin, mid_bin, n_hops, hop, cols }
}

// ── Causal estimates the masks read ─────────────────────────────────────────

/// Per hop×bin trailing median of the previous `h_hops` columns (excluding the
/// current one — fully causal, zero lookahead). Empty prefix ⇒ 0 (new energy
/// passes any mask untouched). Upper median (`sorted[len/2]`), matching the
/// ODF median's convention in `reduce_send`.
fn trailing_median(clip: &Clip, h_hops: usize) -> Vec<f32> {
    let nb = clip.num_bins;
    let mut out = vec![0.0f32; clip.n_hops * nb];
    let mut scratch: Vec<f32> = Vec::with_capacity(h_hops);
    for i in 0..clip.n_hops {
        let lo = i.saturating_sub(h_hops);
        if lo == i {
            continue; // hop 0: no history, h stays 0
        }
        let dst = i * nb;
        for k in 0..nb {
            scratch.clear();
            for j in lo..i {
                scratch.push(clip.cols[j * nb + k]);
            }
            let mid = scratch.len() / 2;
            let (_, m, _) = scratch.select_nth_unstable_by(mid, f32::total_cmp);
            out[dst + k] = *m;
        }
    }
    out
}

/// Per hop×bin median of the CURRENT column over bins k±p_bins (clamped) —
/// the vertical-structure (percussive) estimate. Same-hop, zero lookahead.
fn freq_median(clip: &Clip, p_bins: usize) -> Vec<f32> {
    let nb = clip.num_bins;
    let mut out = vec![0.0f32; clip.n_hops * nb];
    let mut scratch: Vec<f32> = Vec::with_capacity(2 * p_bins + 1);
    for i in 0..clip.n_hops {
        let col = clip.col(i);
        let dst = i * nb;
        for k in 0..nb {
            let lo = k.saturating_sub(p_bins);
            let hi = (k + p_bins + 1).min(nb);
            scratch.clear();
            scratch.extend_from_slice(&col[lo..hi]);
            let mid = scratch.len() / 2;
            let (_, m, _) = scratch.select_nth_unstable_by(mid, f32::total_cmp);
            out[dst + k] = *m;
        }
    }
    out
}

// ── Mask families (D9: exact shape is a prototype output) ───────────────────

#[derive(Clone, Copy, PartialEq)]
enum Mask {
    /// No mask — the live detector as shipped (validation + baseline).
    Baseline,
    /// Spectral subtraction: perc = max(0, col − α·h).
    Sub { alpha: f32, h: usize },
    /// Hard gate: perc = col where col > β·h, else 0.
    Gate { beta: f32, h: usize },
    /// Wiener soft mask: perc = col · p²/(p²+h²); h == 0 ⇒ pass (new energy).
    Wiener { h: usize, p: usize },
    /// dB novelty floor: the column passes UNTOUCHED; instead the per-bin ODF
    /// reference is floored at the trailing median's dB + `margin_db`, so flux
    /// counts only the rise ABOVE the sustained (harmonic) baseline. Flux can
    /// only shrink vs baseline (the reference is a max), so the zero-fire
    /// guards cannot regress by construction; sustained wobble under
    /// h+margin reads 0, and a kick is measured from the bass level up.
    NovFloor { margin_db: f32, h: usize },
    /// Round 3 — the BUG-044 move repeated: the baseline fire path runs
    /// UNCHANGED (guards that pass today cannot regress), OR'd with a third
    /// criterion evaluated on the harmonic-FLOORED flux curve: its candidate
    /// must be a local peak of that curve and dwarf that curve's own recent
    /// max (`> nov_factor × recent_max + delta`). Continuous firers (growl's
    /// filter sweeps, riser, dense beds) keep their floored flux continuous,
    /// so their own recent max suppresses them; a kick is an impulse over a
    /// bass-free floor.
    OrFloor { margin_db: f32, delta: f32, h: usize },
    /// Round 4 — the mechanism the PICTURE showed (bad_guy mix PNG): a mix
    /// kick's surviving low-band evidence is its descending FM sweep
    /// (120→45 Hz over ~90 ms ≈ 2 bins/hop), which SuperFlux's max-filter
    /// nulls BY DESIGN (it exists to ignore bin-sliding energy). Baseline
    /// path unchanged, OR'd with a descending-ridge event: the band's apex
    /// bin must fall coherently — per-hop step in [-step_max, +1] bins, at
    /// least 3 hops — accumulating ≥ `drop_bins` of descent. Fires at the
    /// crossing (~25-40 ms into the sweep), once per run. Static apexes
    /// (bass, sub, growl), slow glides (dive: 0.3 bins/hop), ascents
    /// (riser), and one-hop teleports (bass note changes) all fail the
    /// coherence test.
    Sweep { drop_bins: f32, step_max: f32 },
    /// Round 5 — the v0 successor. v0's four measured failures (P6a round 4):
    /// global argmax sticks to the louder bass ridge, bass portamento
    /// false-fires, the kick double-fires (attack + body), and the pure-kick
    /// synth guard fired 15 for 8. All four are "shape A done naively." This
    /// tracks MULTIPLE ridges (local maxima), not one global apex, so a kick's
    /// descending ridge is followed even while a louder bass ridge sits static;
    /// discriminates by RATE + EXTENT over a window (portamento is too slow to
    /// accumulate `drop_bins` within `win` hops); and OR's into the base flux
    /// path under one SHARED refractory (the attack and body collapse to one
    /// fire). Runs on the LOW band only (Peter's constraint) — Full-band would
    /// fire on `dive`'s spectrum-wide descent. Replaces the masked-novelty
    /// criterion, it does not stack with it: the base path here is plain
    /// SuperFlux (clean tracks) OR the ridge event (bass-heavy tracks).
    RidgeTrack { drop_bins: f32, win: usize, step_max: f32, min_peak: f32 },
    /// Round 6 — RidgeTrack + a birth-attack gate, hunting the short-window
    /// false-fire cost. Short confirmation windows (w6-7) halve the fire
    /// latency and lift ±35 ms recall ~37→53, but admit fast wobble-bass
    /// slides (drop 8 in 6 hops is within an LFO filter-sweep's reach). The
    /// discriminator is the ATTACK: a kick ADDS broadband sub energy at ridge
    /// birth; a wobble slide only REDISTRIBUTES it — and the max-filtered
    /// SuperFlux ODF (already computed per hop) measures exactly "new energy,
    /// motion excluded." A ridge may only fire if the Low-band ODF spiked
    /// ≥ `gate` within its first ~3 hops (±2 hops of birth for VQT smear).
    /// `gate` is in ODF units (band positive-dB sum: attacks are tens to
    /// hundreds, wobble under the max-filter is near zero).
    RidgeGate { drop_bins: f32, win: usize, step_max: f32, min_peak: f32, gate: f32 },
}

impl Mask {
    fn name(self) -> String {
        match self {
            Mask::Baseline => "baseline".into(),
            Mask::Sub { alpha, h } => format!("S a={alpha:.1} H={h}"),
            Mask::Gate { beta, h } => format!("G b={beta:.1} H={h}"),
            Mask::Wiener { h, p } => format!("W H={h} P={p}"),
            Mask::NovFloor { margin_db, h } => format!("N m={margin_db:.1} H={h}"),
            Mask::OrFloor { margin_db, delta, h } => format!("O m={margin_db:.1} d={delta:.0} H={h}"),
            Mask::Sweep { drop_bins, step_max } => format!("K d={drop_bins:.0} s={step_max:.0}"),
            Mask::RidgeTrack { drop_bins, win, step_max, min_peak } => {
                format!("R d={drop_bins:.0} w={win} s={step_max:.0} p={min_peak:.2}")
            }
            Mask::RidgeGate { drop_bins, win, gate, .. } => {
                format!("RG d={drop_bins:.0} w={win} g={gate:.0}")
            }
        }
    }
    fn h_window(self) -> Option<usize> {
        match self {
            Mask::Baseline | Mask::Sweep { .. } | Mask::RidgeTrack { .. } | Mask::RidgeGate { .. } => None,
            Mask::Sub { h, .. }
            | Mask::Gate { h, .. }
            | Mask::Wiener { h, .. }
            | Mask::NovFloor { h, .. }
            | Mask::OrFloor { h, .. } => Some(h),
        }
    }
    fn p_window(self) -> Option<usize> {
        match self {
            Mask::Wiener { p, .. } => Some(p),
            _ => None,
        }
    }
    fn sweep_params(self) -> Option<(f32, f32)> {
        match self {
            Mask::Sweep { drop_bins, step_max } => Some((drop_bins, step_max)),
            _ => None,
        }
    }
    fn apply(self, col: f32, h: f32, p: f32) -> f32 {
        match self {
            Mask::Baseline => col,
            Mask::Sub { alpha, .. } => (col - alpha * h).max(0.0),
            Mask::Gate { beta, .. } => {
                if col > beta * h {
                    col
                } else {
                    0.0
                }
            }
            Mask::Wiener { .. } => {
                if h <= 1e-12 {
                    col
                } else {
                    col * (p * p) / (p * p + h * h)
                }
            }
            // NovFloor/OrFloor/Sweep don't transform the column — they modify
            // the ODF reference / fire criteria inside the replay.
            Mask::NovFloor { .. }
            | Mask::OrFloor { .. }
            | Mask::Sweep { .. }
            | Mask::RidgeTrack { .. }
            | Mask::RidgeGate { .. } => col,
        }
    }
    /// (drop_bins, win, step_max, min_peak, attack_gate); gate 0.0 = ungated.
    fn ridge_params(self) -> Option<(f32, usize, f32, f32, f32)> {
        match self {
            Mask::RidgeTrack { drop_bins, win, step_max, min_peak } => {
                Some((drop_bins, win, step_max, min_peak, 0.0))
            }
            Mask::RidgeGate { drop_bins, win, step_max, min_peak, gate } => {
                Some((drop_bins, win, step_max, min_peak, gate))
            }
            _ => None,
        }
    }
}

/// Precomputed estimates for one clip, keyed by window size.
struct Estimates {
    h: std::collections::HashMap<usize, Vec<f32>>,
    p: std::collections::HashMap<usize, Vec<f32>>,
}

impl Estimates {
    fn build(clip: &Clip, masks: &[Mask]) -> Self {
        let mut h = std::collections::HashMap::new();
        let mut p = std::collections::HashMap::new();
        for m in masks {
            if let Some(w) = m.h_window() {
                h.entry(w).or_insert_with(|| trailing_median(clip, w));
            }
            if let Some(w) = m.p_window() {
                p.entry(w).or_insert_with(|| freq_median(clip, w));
            }
        }
        Self { h, p }
    }
}

// ── Fire replay — exact replica of reduce_send's ODF + fire logic ───────────

/// SuperFlux ODF for one band range over (masked) current/previous columns:
/// positive dB rise vs the previous column's ±MAXFILTER_RADIUS max, clamped
/// to the colour-ramp window — byte-for-byte the loop in `band_reduce`.
fn superflux(cur: &[f32], prev: &[f32], lo: usize, hi: usize, db_min: f32, db_max: f32) -> f32 {
    let hi = hi.min(cur.len());
    if lo >= hi {
        return 0.0;
    }
    let nb = prev.len();
    let mut sf = 0.0f32;
    for (k, &cv) in cur.iter().enumerate().take(hi).skip(lo) {
        let klo = k.saturating_sub(MAXFILTER_RADIUS);
        let khi = (k + MAXFILTER_RADIUS + 1).min(nb);
        let mut prev_max = 0.0f32;
        for &pv in &prev[klo..khi] {
            if pv > prev_max {
                prev_max = pv;
            }
        }
        let m_db = (20.0 * cv.max(1e-9).log10()).clamp(db_min, db_max);
        let prev_db = (20.0 * prev_max.max(1e-9).log10()).clamp(db_min, db_max);
        let ds = m_db - prev_db;
        if ds > 0.0 {
            sf += ds;
        }
    }
    sf
}

/// SuperFlux ODF with a per-bin dB novelty floor (the NovFloor family): the
/// rise reference is `max(prev ±1-bin max, h_est + margin)` in dB — flux is
/// counted only above BOTH the previous column and the trailing-median
/// (harmonic) baseline. Strictly ≤ the plain `superflux` value per bin.
fn superflux_floor(
    cur: &[f32],
    prev: &[f32],
    h_row: &[f32],
    margin_db: f32,
    lo: usize,
    hi: usize,
    db_min: f32,
    db_max: f32,
) -> f32 {
    let hi = hi.min(cur.len());
    if lo >= hi {
        return 0.0;
    }
    let nb = prev.len();
    let mut sf = 0.0f32;
    for (k, &cv) in cur.iter().enumerate().take(hi).skip(lo) {
        let klo = k.saturating_sub(MAXFILTER_RADIUS);
        let khi = (k + MAXFILTER_RADIUS + 1).min(nb);
        let mut prev_max = 0.0f32;
        for &pv in &prev[klo..khi] {
            if pv > prev_max {
                prev_max = pv;
            }
        }
        let m_db = (20.0 * cv.max(1e-9).log10()).clamp(db_min, db_max);
        let prev_db = (20.0 * prev_max.max(1e-9).log10()).clamp(db_min, db_max);
        let h_db = (20.0 * h_row[k].max(1e-9).log10()).clamp(db_min, db_max) + margin_db;
        let ds = m_db - prev_db.max(h_db);
        if ds > 0.0 {
            sf += ds;
        }
    }
    sf
}

/// One followed ridge for the RidgeTrack criterion: the last `win` apex bins
/// (newest at back), a gap counter (hops since last extension), and a
/// once-fired latch so a single descent can't re-fire while it keeps descending.
struct Track {
    bins: VecDeque<f32>,
    gap: u8,
    fired: bool,
    birth: usize,
    /// Max Low-band ODF seen in the ridge's birth window (±2 hops before birth
    /// via the ODF history, first 3 extensions after). RidgeGate's fire
    /// criterion; 0-cost for RidgeTrack.
    attack: f32,
    /// Full apex-bin history since birth (miss-audit only; the fire logic never
    /// reads it). Gap hops don't push, so index ≈ hops-since-birth ± gap slop.
    hist: Vec<f32>,
}

/// One dead (or end-of-clip) Low-band track's lifecycle, for `--miss-audit`:
/// classify each missed label by what its nearest ridge actually did.
struct RidgeDiag {
    birth: usize,
    hist: Vec<f32>,
    fired: bool,
    attack: f32,
}

/// Replay the per-band fire logic over a clip with `mask` applied to the ODF's
/// input columns (and nothing else). Returns fire hop indices per band
/// [Full, Low, Mid, High]. `have_prev` arms at hop 16, matching the live
/// window-fill guard; the ODF history ring only accumulates from there —
/// both exactly as `reduce_send` behaves under mod_harness's feed cadence.
/// Prototype-only measurement switch (set once from `--ridge-only` in `main`):
/// when true, the Low band fires SOLELY on the ridge criterion — the base flux
/// path and its recent-fire dedup are suppressed for bi==1. Models the planned
/// no-fallback `Kick` feature (ridge-only) vs the shipped hybrid Transients@Low
/// (flux OR ridge). Not shared engine state — a throwaway example toggle.
static RIDGE_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Upper bound of the per-hop step gate (bins, f32 bits; default +1.0). The
/// descent gate is [-step_max, STEP_UP]. +1 tolerates VQT jitter on a real
/// sweep; in a dense noise-peak field (riser mid-sweep, snare tails) the
/// asymmetric gate lets a track random-walk DOWNWARD — at short confirmation
/// windows that walk reaches drop_bins and false-fires (riser Low 2→13 at
/// d10/w6). 0/-1 demand a never-rising / strictly-falling ridge. `--stepup`.
static RIDGE_STEP_UP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0x3F80_0000); // 1.0f32

/// Absolute peak floor (tilted-column units; default 0 = off, `--absfloor`).
/// The relative floor (`min_peak` × band max) scales DOWN in quiet passages —
/// after a riser's noise band ascends out of Low, the residual filter-skirt
/// ripple is near-silent yet its local maxima still clear a relative floor,
/// and a short-window track can random-walk down through them (riser Low
/// 2→13 at d10/w6). A kick apex is loud in ABSOLUTE terms; skirt ripple is
/// not. Peaks must clear BOTH floors.
static RIDGE_ABS_FLOOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn replay_fires(clip: &Clip, mask: Mask, est: &Estimates) -> [Vec<usize>; 4] {
    replay_fires_dump(clip, mask, est, None, None)
}

/// Same replay with an optional per-hop trace: one CSV line per hop with each
/// band's candidate / median-threshold / novelty-ref / is_peak / refractory /
/// fired — the "which test blocked this kick" instrument.
fn replay_fires_dump(
    clip: &Clip,
    mask: Mask,
    est: &Estimates,
    mut dump: Option<&mut Vec<String>>,
    mut ridge_diag: Option<&mut Vec<RidgeDiag>>,
) -> [Vec<usize>; 4] {
    let cfg = SpectrogramConfig::default();
    let (db_min, db_max) = (cfg.db_min, cfg.db_max);
    let nb = clip.num_bins;
    let bands = [
        (0, nb),
        (0, clip.low_bin),
        (clip.low_bin, clip.mid_bin),
        (clip.mid_bin, nb),
    ];
    let h_arr = mask.h_window().map(|w| &est.h[&w]);
    let p_arr = mask.p_window().map(|w| &est.p[&w]);

    let mut hist = [[0.0f32; ODF_MEDIAN_HOPS]; 4];
    // Second ODF history for the OrFloor family: the harmonic-floored flux
    // curve, with its own candidate/peak/recent-max bookkeeping.
    let mut fhist = [[0.0f32; ODF_MEDIAN_HOPS]; 4];
    // Sweep family: ring of the band's apex bin over the last SWEEP_WIN hops
    // (newest last; -1 = band empty that hop), plus a once-per-descent latch.
    const SWEEP_WIN: usize = 12;
    let mut apex_hist = [[-1.0f32; SWEEP_WIN]; 4];
    let mut sweep_latch = [false; 4];
    // RidgeTrack family: a small set of followed ridges per band (only the Low
    // band, index 1, is ever populated — kicks are a Low-band event and a
    // Full-band tracker would fire on `dive`'s spectrum-wide descent).
    const MAX_TRACKS: usize = 12;
    const MAX_GAP: u8 = 1;
    let mut tracks: [Vec<Track>; 4] = Default::default();
    let mut refractory = [0u8; 4];
    // Last hop any criterion fired in each band — the RidgeTrack criterion
    // dedups against it: a ridge (kick BODY) that confirms within `win` hops of
    // a prior fire is the body of an already-reported attack, so it's silenced.
    // Where flux went deaf (bass-heavy Low band) there is no prior fire, so the
    // ridge speaks — it is precisely the fallback for the missed attack.
    let mut last_fire = [-1000i64; 4];
    let mut fires: [Vec<usize>; 4] = Default::default();
    let mut prev_perc = vec![0.0f32; nb];
    let mut cur_perc = vec![0.0f32; nb];

    for i in 0..clip.n_hops {
        let col = clip.col(i);
        for k in 0..nb {
            let h = h_arr.map_or(0.0, |a| a[i * nb + k]);
            let p = p_arr.map_or(0.0, |a| a[i * nb + k]);
            cur_perc[k] = mask.apply(col[k], h, p);
        }
        if i >= 16 {
            let mut row = dump.as_ref().map(|_| format!("{i}"));
            // have_prev: the analysis window has filled (4096/256 = 16 hops).
            for (bi, &(lo, hi)) in bands.iter().enumerate() {
                let odf = if let Mask::NovFloor { margin_db, .. } = mask {
                    let h_row = &h_arr.unwrap()[i * nb..(i + 1) * nb];
                    superflux_floor(&cur_perc, &prev_perc, h_row, margin_db, lo, hi, db_min, db_max)
                } else {
                    superflux(&cur_perc, &prev_perc, lo, hi, db_min, db_max)
                };
                let h = &hist[bi];
                let candidate = h[ODF_MEDIAN_HOPS - 1];
                let mut sorted = *h;
                sorted.sort_unstable_by(f32::total_cmp);
                let median = sorted[ODF_MEDIAN_HOPS / 2];
                let threshold = median * SUPERFLUX_THRESH_FACTOR + SUPERFLUX_DELTA;
                let lookback_lo = ODF_MEDIAN_HOPS - 1 - ODF_PEAK_LOOKBACK;
                let past_max =
                    h[lookback_lo..ODF_MEDIAN_HOPS - 1].iter().copied().fold(0.0f32, f32::max);
                let is_peak = candidate >= past_max && odf <= candidate;
                let novelty_ref =
                    h[ODF_NOVELTY_LO..ODF_NOVELTY_HI].iter().copied().fold(0.0f32, f32::max);
                let novel =
                    candidate > novelty_ref * SUPERFLUX_NOVELTY_FACTOR + SUPERFLUX_NOVELTY_DELTA;
                let mut fired = is_peak && refractory[bi] == 0 && (candidate > threshold || novel);
                // OrFloor third criterion, on the floored curve's own terms.
                if let Mask::OrFloor { margin_db, delta, .. } = mask {
                    let h_row = &h_arr.unwrap()[i * nb..(i + 1) * nb];
                    let fodf = superflux_floor(
                        &cur_perc, &prev_perc, h_row, margin_db, lo, hi, db_min, db_max,
                    );
                    let fh = &fhist[bi];
                    let fcand = fh[ODF_MEDIAN_HOPS - 1];
                    let fpast_max =
                        fh[lookback_lo..ODF_MEDIAN_HOPS - 1].iter().copied().fold(0.0f32, f32::max);
                    let f_is_peak = fcand >= fpast_max && fodf <= fcand;
                    let f_ref =
                        fh[ODF_NOVELTY_LO..ODF_NOVELTY_HI].iter().copied().fold(0.0f32, f32::max);
                    let f_novel = fcand > f_ref * SUPERFLUX_NOVELTY_FACTOR + delta;
                    fired = fired || (f_is_peak && refractory[bi] == 0 && f_novel);
                    let fm = &mut fhist[bi];
                    fm.copy_within(1.., 0);
                    fm[ODF_MEDIAN_HOPS - 1] = fodf;
                }
                // Sweep fourth criterion: coherent descending apex run.
                if let Some((drop_bins, step_max)) = mask.sweep_params() {
                    let col = clip.col(i);
                    let (mut best_k, mut best_v) = (-1.0f32, 0.0f32);
                    for (k, &cv) in col.iter().enumerate().take(hi.min(nb)).skip(lo) {
                        if cv > best_v {
                            best_v = cv;
                            best_k = k as f32;
                        }
                    }
                    let ah = &mut apex_hist[bi];
                    ah.copy_within(1.., 0);
                    ah[SWEEP_WIN - 1] = best_k;
                    // Coherent = every hop present and every step within
                    // [-step_max, +1] bins; the test is NET descent across the
                    // window, so static-apex jitter can't accumulate and slow
                    // glides (dive: ~0.3 bins/hop) can't reach the drop.
                    let mut coherent = ah.iter().all(|&a| a >= 0.0);
                    if coherent {
                        for w in 1..SWEEP_WIN {
                            let step = ah[w] - ah[w - 1];
                            if !(-step_max..=1.0).contains(&step) {
                                coherent = false;
                                break;
                            }
                        }
                    }
                    if !coherent {
                        sweep_latch[bi] = false; // descent broke: re-arm
                    } else if ah[0] - ah[SWEEP_WIN - 1] >= drop_bins
                        && !sweep_latch[bi]
                        && refractory[bi] == 0
                    {
                        fired = true;
                        sweep_latch[bi] = true; // once per descent
                    }
                }
                // RidgeTrack criterion: multi-ridge descent, Low band only.
                if let Some((drop_bins, win, step_max, min_peak, attack_gate)) =
                    mask.ridge_params().filter(|_| bi == 1)
                {
                    {
                        let step_up =
                            f32::from_bits(RIDGE_STEP_UP.load(std::sync::atomic::Ordering::Relaxed));
                        let col = clip.col(i);
                        // Peak-pick: local maxima above a fraction of the band max.
                        let band_max = col[lo..hi].iter().copied().fold(0.0f32, f32::max);
                        let abs_floor = f32::from_bits(
                            RIDGE_ABS_FLOOR.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        let floor = (band_max * min_peak).max(abs_floor);
                        let mut peaks: Vec<usize> = Vec::new();
                        let plo = lo.max(1);
                        for k in plo..hi.saturating_sub(1) {
                            let v = col[k];
                            if v >= floor && v > col[k - 1] && v >= col[k + 1] {
                                peaks.push(k);
                            }
                        }
                        let mut consumed = vec![false; peaks.len()];
                        let tks = &mut tracks[bi];
                        // Extend each track with the nearest unconsumed peak in
                        // the descent gate [last - step_max, last + 1].
                        for tk in tks.iter_mut() {
                            let last = *tk.bins.back().unwrap();
                            let mut best_j: Option<usize> = None;
                            let mut best_d = f32::INFINITY;
                            for (j, &pk) in peaks.iter().enumerate() {
                                if consumed[j] {
                                    continue;
                                }
                                let d = pk as f32 - last;
                                if (-step_max..=step_up).contains(&d) && d.abs() < best_d {
                                    best_d = d.abs();
                                    best_j = Some(j);
                                }
                            }
                            if let Some(j) = best_j {
                                consumed[j] = true;
                                tk.bins.push_back(peaks[j] as f32);
                                if tk.bins.len() > win {
                                    tk.bins.pop_front();
                                }
                                tk.gap = 0;
                                tk.hist.push(peaks[j] as f32);
                                // Birth-attack window: the ODF spike may lag the
                                // ridge birth by a hop or two (VQT smear).
                                if tk.hist.len() <= 3 {
                                    tk.attack = tk.attack.max(odf);
                                }
                            } else {
                                tk.gap += 1;
                            }
                        }
                        // Fire: a full window that descended >= drop_bins
                        // coherently, once per descent, shared refractory.
                        // Age cap: the descent must be the ridge's whole short
                        // life (born at the attack, descends, dies). A bass
                        // portamento is a long-lived ridge that bends late — its
                        // age at the bend far exceeds `win`, so it's rejected
                        // here without any rate/extent overlap with a kick.
                        let age_cap = win + 6;
                        let mut ridge_fire = false;
                        for tk in tks.iter_mut() {
                            if tk.fired || tk.gap != 0 || tk.bins.len() < win || i - tk.birth > age_cap
                            {
                                continue;
                            }
                            // Birth-attack gate (RidgeGate only): no new sub
                            // energy near birth ⇒ a slide, not a kick.
                            if attack_gate > 0.0 && tk.attack < attack_gate {
                                continue;
                            }
                            let front = *tk.bins.front().unwrap();
                            let back = *tk.bins.back().unwrap();
                            if front - back < drop_bins {
                                continue;
                            }
                            let coherent = (1..tk.bins.len())
                                .all(|w| {
                                    (-step_max..=step_up)
                                        .contains(&(tk.bins[w] - tk.bins[w - 1]))
                                });
                            if coherent {
                                tk.fired = true;
                                ridge_fire = true;
                            }
                        }
                        // Cull broken tracks; birth new ones from stray peaks.
                        if let Some(diag) = ridge_diag.as_deref_mut() {
                            for tk in tks.iter().filter(|tk| tk.gap > MAX_GAP) {
                                diag.push(RidgeDiag {
                                    birth: tk.birth,
                                    hist: tk.hist.clone(),
                                    fired: tk.fired,
                                    attack: tk.attack,
                                });
                            }
                        }
                        tks.retain(|tk| tk.gap <= MAX_GAP);
                        // Birth attack: current hop's ODF plus the two before it
                        // (hist push happens after this block, so 15/14 are the
                        // previous two hops).
                        let birth_attack =
                            odf.max(hist[bi][ODF_MEDIAN_HOPS - 1]).max(hist[bi][ODF_MEDIAN_HOPS - 2]);
                        for (j, &pk) in peaks.iter().enumerate() {
                            if !consumed[j] {
                                let mut b = VecDeque::with_capacity(win);
                                b.push_back(pk as f32);
                                tks.push(Track {
                                    bins: b,
                                    gap: 0,
                                    fired: false,
                                    birth: i,
                                    attack: birth_attack,
                                    hist: vec![pk as f32],
                                });
                            }
                        }
                        if tks.len() > MAX_TRACKS {
                            let drop_n = tks.len() - MAX_TRACKS;
                            if let Some(diag) = ridge_diag.as_deref_mut() {
                                for tk in &tks[..drop_n] {
                                    diag.push(RidgeDiag {
                                        birth: tk.birth,
                                        hist: tk.hist.clone(),
                                        fired: tk.fired,
                                        attack: tk.attack,
                                    });
                                }
                            }
                            tks.drain(0..drop_n);
                        }
                        // OR into the base fire, but dedup against the attack:
                        // suppress if the same band already fired this hop (flux
                        // saw the attack now) or within the confirmation window
                        // (flux saw the attack ~win hops ago). `+3` covers the
                        // VQT-kernel smear between the true attack and the ridge
                        // reaching drop_bins.
                        if RIDGE_ONLY.load(std::sync::atomic::Ordering::Relaxed) {
                            // No-fallback Kick model: the ridge IS the detector.
                            // Drop the base-flux fire and the flux-dedup window;
                            // the per-track `fired` latch + shared refractory
                            // already give one fire per descent.
                            fired = ridge_fire && refractory[bi] == 0;
                        } else {
                            let recent_fire =
                                fired || (i as i64 - last_fire[bi]) < win as i64 + 3;
                            if ridge_fire && refractory[bi] == 0 && !recent_fire {
                                fired = true;
                            }
                        }
                    }
                }
                if let Some(r) = row.as_mut() {
                    r.push_str(&format!(
                        ",{candidate:.1},{threshold:.1},{novelty_ref:.1},{},{},{}",
                        u8::from(is_peak),
                        refractory[bi],
                        u8::from(fired)
                    ));
                }
                if fired {
                    fires[bi].push(i);
                    refractory[bi] = ONSET_REFRACTORY_HOPS;
                    last_fire[bi] = i as i64;
                } else {
                    refractory[bi] = refractory[bi].saturating_sub(1);
                }
                let hm = &mut hist[bi];
                hm.copy_within(1.., 0);
                hm[ODF_MEDIAN_HOPS - 1] = odf;
            }
            if let (Some(d), Some(r)) = (dump.as_deref_mut(), row) {
                d.push(r);
            }
        }
        std::mem::swap(&mut prev_perc, &mut cur_perc);
    }
    // Flush tracks still alive at end-of-clip so the miss-audit sees them.
    if let Some(diag) = ridge_diag {
        for tk in tracks[1].iter() {
            diag.push(RidgeDiag {
                birth: tk.birth,
                hist: tk.hist.clone(),
                fired: tk.fired,
                attack: tk.attack,
            });
        }
    }
    fires
}

// ── Metrics ──────────────────────────────────────────────────────────────────

fn count_post_warmup(fires: &[usize]) -> usize {
    fires.iter().filter(|&&i| i >= WARMUP_HOPS).count()
}

fn matched_within(t: f32, refs: &[f32], tol_s: f32) -> bool {
    refs.iter().any(|&r| (t - r).abs() <= tol_s)
}

/// Load one track's kick labels: (mix_time_s, drums_time_s) columns from
/// tests/fixtures/audio_labels/<track>.csv. The 73 hand-verified events are the
/// grading target (README provenance) — mix fires grade against mix_time_s,
/// drums fires against drums_time_s. Missing file ⇒ empty (that track ungraded).
fn load_labels(track: &str) -> (Vec<f32>, Vec<f32>) {
    let path = format!("tests/fixtures/audio_labels/{track}.csv");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("no labels: {path}");
        return (Vec::new(), Vec::new());
    };
    let (mut mix, mut drums) = (Vec::new(), Vec::new());
    for line in text.lines().skip(1) {
        let mut it = line.split(',');
        let (Some(m), Some(d)) = (it.next(), it.next()) else { continue };
        if let (Ok(m), Ok(d)) = (m.trim().parse::<f32>(), d.trim().parse::<f32>()) {
            mix.push(m);
            drums.push(d);
        }
    }
    (mix, drums)
}

/// Grade fire times against label times: (recovered@±35ms, recovered@±70ms,
/// spurious = fires >±70ms from any label). Precision/recall derive from these.
fn grade(fires: &[f32], labels: &[f32]) -> (usize, usize, usize) {
    let rec35 = labels.iter().filter(|&&l| matched_within(l, fires, 0.035)).count();
    let rec70 = labels.iter().filter(|&&l| matched_within(l, fires, 0.070)).count();
    let spurious = fires.iter().filter(|&&f| !matched_within(f, labels, 0.070)).count();
    (rec35, rec70, spurious)
}

/// Signed fire latency per recovered label: for each label with a fire within
/// ±70 ms, the nearest fire's offset in ms (positive = fire AFTER the label's
/// attack). The labels mark the attack (25% sub-envelope walk-back), so this is
/// the detector's true latency — the metric the recall counts hide: a fire at
/// +60 ms "recovers" the label at ±70 but reads as sloppy on stage.
fn latencies_ms(fires: &[f32], labels: &[f32]) -> Vec<f32> {
    labels
        .iter()
        .filter_map(|&l| {
            fires
                .iter()
                .map(|&f| f - l)
                .filter(|d| d.abs() <= 0.070)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        })
        .map(|d| d * 1000.0)
        .collect()
}

/// Classify one missed label from the Low-band ridge lifecycles (`--miss-audit`).
/// The extension gate already enforces per-step coherence, so a miss is one of:
/// no ridge born at the attack (peak-pick/floor), born too late (apex initially
/// merged with a louder bass peak), killed by a >1-hop gap, too shallow a
/// descent, fired-but-late, or reached the drop yet was swallowed
/// (latch/refractory/age-cap). `hop_l` is the label time in hops.
fn classify_miss(hop_l: f32, diags: &[RidgeDiag], drop_bins: f32, win: usize, gate: f32) -> String {
    // Best coherent descent within any ≤win-length slice of a track's history.
    let best_drop = |hist: &[f32]| -> f32 {
        let mut best = 0.0f32;
        for i in 0..hist.len() {
            for j in (i + 1)..hist.len().min(i + win) {
                best = best.max(hist[i] - hist[j]);
            }
        }
        best
    };
    let born_in = |lo: f32, hi: f32| {
        diags.iter().filter(move |d| {
            let b = d.birth as f32;
            b >= hop_l + lo && b <= hop_l + hi
        })
    };
    let candidates: Vec<&RidgeDiag> = born_in(-4.0, 10.0).collect();
    if candidates.is_empty() {
        return if born_in(10.0, 17.0).next().is_some() {
            "born-late (apex merged with bass at attack)".into()
        } else {
            "no-birth (no Low-band local max above floor)".into()
        };
    }
    if candidates.iter().any(|d| d.fired) {
        return "fired-late (>70ms after label)".into();
    }
    let best = candidates
        .iter()
        .max_by(|a, b| best_drop(&a.hist).total_cmp(&best_drop(&b.hist)))
        .unwrap();
    let (bd, len) = (best_drop(&best.hist), best.hist.len());
    if len < win {
        format!("gap-death (len {len} < win {win}, drop {bd:.0})")
    } else if bd < drop_bins {
        format!("shallow (drop {bd:.0} < {drop_bins:.0}, len {len})")
    } else if gate > 0.0 && best.attack < gate {
        format!("gated (attack {:.0} < {gate:.0}, drop {bd:.0})", best.attack)
    } else {
        format!("swallowed (drop {bd:.0} reached; latch/refractory/age-cap)")
    }
}

/// (median, p90) of a latency sample, or None if empty.
fn latency_stats(mut ms: Vec<f32>) -> Option<(f32, f32)> {
    if ms.is_empty() {
        return None;
    }
    ms.sort_unstable_by(f32::total_cmp);
    let med = ms[ms.len() / 2];
    let p90 = ms[(ms.len() * 9 / 10).min(ms.len() - 1)];
    Some((med, p90))
}

// ── Synth scenarios, copied verbatim from mod_harness.rs (guard replay) ─────

const SELFTEST_SR: u32 = 48_000;
const SELFTEST_SECS: usize = 4;

fn selftest_buf() -> Vec<f32> {
    vec![0.0f32; SELFTEST_SECS * SELFTEST_SR as usize]
}

fn soft_clip(mut v: Vec<f32>) -> Vec<f32> {
    for s in &mut v {
        *s = s.tanh();
    }
    v
}

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

fn synth_riser() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let secs = SELFTEST_SECS as f32;
    let nyquist = srf * 0.45;
    let mut seed = 0xACE1_u32;
    let mut hp_lp_state = 0.0f32;
    let mut bp_state = 0.0f32;
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

fn synth_dense_bed() -> Vec<f32> {
    let mut out = selftest_buf();
    let srf = SELFTEST_SR as f32;
    let detunes: [f32; 7] = [-0.12, -0.08, -0.04, 0.0, 0.04, 0.08, 0.12];
    let clusters: [(f32, f32); 3] = [(55.0, 0.24), (500.0, 0.05), (2200.0, 0.035)];
    let mut lfo_rate = [[0.0f32; 7]; 3];
    let mut lfo_phase = [[0.0f32; 7]; 3];
    for ci in 0..3 {
        for vi in 0..7 {
            let h = (ci * 7 + vi) as f32;
            lfo_rate[ci][vi] = 3.0 + 5.0 * (h * 0.37).fract();
            lfo_phase[ci][vi] = (h * 0.61).fract();
        }
    }
    let mut phases = [[0.0f32; 7]; 3];
    let mut seed = 0x9E3779B9u32;
    let mut prev_noise = 0.0f32;
    for (i, s_out) in out.iter_mut().enumerate() {
        let t = i as f32 / srf;
        let mut s = 0.0f32;
        for (ci, &(base, gain)) in clusters.iter().enumerate() {
            for vi in 0..7 {
                let f = base * 2.0f32.powf(detunes[vi] / 12.0);
                let p = &mut phases[ci][vi];
                *p = (*p + f / srf).fract();
                let lfo =
                    (std::f32::consts::TAU * (lfo_rate[ci][vi] * t + lfo_phase[ci][vi])).sin();
                let amp = 1.0 + 0.6 * lfo;
                s += gain * amp * (2.0 * *p - 1.0);
            }
        }
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
        let bright = noise - prev_noise;
        prev_noise = noise;
        s += 0.10 * bright;
        *s_out += s;
    }
    out
}

// ── Main ─────────────────────────────────────────────────────────────────────

const TRACKS: [&str; 5] = [
    "apricots_128bpm",
    "bad_guy_128bpm",
    "feel_the_vibration_174bpm",
    "inhale_exhale_145bpm",
    "tears_140bpm",
];
const STEMS: [&str; 5] = ["mix", "drums", "bass", "others", "vocals"];

fn decode_mono(path: &str) -> Option<(Vec<f32>, u32)> {
    let decoded = match manifold_playback::audio_decoder::decode_audio_to_pcm(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip {path}: {e}");
            return None;
        }
    };
    let ch = decoded.channels.max(1);
    let mono: Vec<f32> =
        decoded.samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    Some((mono, decoded.sample_rate))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let validate_only = args.iter().any(|a| a == "--validate-only");
    // Ridge-only measurement (no-fallback Kick model): Low band fires solely on
    // the ridge criterion. Pair with `--family ridge` or `--family ridge-sweep`.
    if args.iter().any(|a| a == "--ridge-only") {
        RIDGE_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(v) = args
        .iter()
        .position(|a| a == "--stepup")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f32>().ok())
    {
        RIDGE_STEP_UP.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(v) = args
        .iter()
        .position(|a| a == "--absfloor")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f32>().ok())
    {
        RIDGE_ABS_FLOOR.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
    let fixtures_root = args
        .iter()
        .position(|a| a == "--fixtures")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "tests/fixtures/audio".into());
    let low_hz = manifold_core::audio_setup::DEFAULT_LOW_HZ;
    let mid_hz = manifold_core::audio_setup::DEFAULT_MID_HZ;

    // ── The sweep grid: bounded candidates, mechanism-justified (D9) ────────
    let mut masks: Vec<Mask> = vec![Mask::Baseline];
    // Round 1 (recorded in the P6a report): Sub/Gate/Wiener column masks all
    // failed — Sub/Gate recover kicks only via mask flutter (growl 16–73 false
    // fires, spurious 30–70/clip); Wiener only rescales, and dB flux is
    // scale-invariant, so it changes almost nothing. Round 2 sweeps the
    // NovFloor family (per-bin dB reference floor), which cannot regress the
    // zero-fire guards by construction.
    // Round 2 (also recorded): NovFloor as a REPLACEMENT ODF recovers real
    // kicks but collapses the adaptive median's context, so growl's filter-
    // sweep spikes fire against a floor threshold (0 → 62-73). Round 3 ORs
    // the floored curve in as a third criterion with its own recent-max
    // reference — the baseline path (and every guard it already passes) is
    // untouched by construction.
    // Grid selection: `--family sub|gate|wiener|nov|or|sweep|all` re-runs any
    // round of the P6a campaign (all four rounds' tables live in the phase
    // report; every family measured 2026-07-06 FAILED the bad_guy ≥50% bar):
    //   round 1  sub/gate  — recover via mask flutter; growl 16-73 false fires
    //   round 1  wiener    — dB flux is scale-invariant; changes ~nothing
    //   round 2  nov       — replacement ODF collapses the adaptive median's
    //                        context; growl 0→62-73
    //   round 3  or        — guard-green by construction, best partial result
    //                        (apricots 12/13, feel 16/35, bad_guy only 8/45)
    //   round 4  sweep     — apex sticks to the louder bass; bass portamento
    //                        false-fires; kicks double-fire (attack + body)
    let family = args
        .iter()
        .position(|a| a == "--family")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "or".into());
    if !validate_only {
        let all = family == "all";
        if all || family == "sub" || family == "gate" || family == "wiener" {
            for &h in &[16usize, 32, 64] {
                for &alpha in &[1.0f32, 1.5, 2.0] {
                    if all || family == "sub" {
                        masks.push(Mask::Sub { alpha, h });
                    }
                }
                for &beta in &[1.5f32, 2.0, 3.0] {
                    if all || family == "gate" {
                        masks.push(Mask::Gate { beta, h });
                    }
                }
                for &p in &[6usize, 12, 18] {
                    if all || family == "wiener" {
                        masks.push(Mask::Wiener { h, p });
                    }
                }
            }
        }
        if all || family == "nov" {
            for &h in &[16usize, 32, 64] {
                for &margin_db in &[3.0f32, 4.5, 6.0, 9.0] {
                    masks.push(Mask::NovFloor { margin_db, h });
                }
            }
        }
        if all || family == "or" {
            for &h in &[16usize, 32] {
                for &margin_db in &[3.0f32, 4.5, 6.0] {
                    for &delta in &[80.0f32, 125.0, 200.0] {
                        masks.push(Mask::OrFloor { margin_db, delta, h });
                    }
                }
            }
        }
        // Fine plateau scan around the round-3 winner (m=3, H=16): delta
        // 80→125 was a cliff (feel 16→5), so 80 needs plateau evidence on
        // its low side before it ships as a constant.
        if family == "or-plateau" {
            for &delta in &[40.0f32, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0] {
                masks.push(Mask::OrFloor { margin_db: 3.0, delta, h: 16 });
            }
        }
        // The shipping config (BUG-046 partial, Peter-approved 2026-07-06):
        // prints the exact per-clip per-band fire counts the runtime
        // integration must reproduce (the prototype is the reference
        // implementation — the integration gate is an EXACT match).
        if family == "or-final" {
            masks.push(Mask::OrFloor { margin_db: 3.0, delta: 80.0, h: 16 });
        }
        if all || family == "sweep" {
            for &drop_bins in &[6.0f32, 8.0, 12.0] {
                for &step_max in &[4.0f32, 6.0] {
                    masks.push(Mask::Sweep { drop_bins, step_max });
                }
            }
        }
        // Round 5 — the v0 successor. Rate/extent bounds are mechanism-derived:
        // a 120→45 Hz kick sweep spans ~34 bins (bpo=24) over ~17 hops ≈ 2
        // bins/hop, so a real descent clears ~24 bins in a 12-hop window; bass
        // portamento (<1 bin/hop) cannot. Grid brackets that: drop 14/18/22
        // over win 10/14, step_max 4 (2 bins/hop + slop), peak floor 6%/12% of
        // the band max (catch the sweep tail under a louder bass without
        // admitting shelf ripple).
        if all || family == "ridge" {
            for &drop_bins in &[14.0f32, 18.0, 22.0] {
                for &win in &[10usize, 14] {
                    for &min_peak in &[0.06f32, 0.12] {
                        masks.push(Mask::RidgeTrack { drop_bins, win, step_max: 4.0, min_peak });
                    }
                }
            }
        }
        // The chosen successor config (spike 2026-07-07): replaces the
        // masked-novelty OrFloor criterion. Nearly 2x its kick recall at equal
        // bass-false-fire cost; all guards green. Prints the exact per-band fire
        // counts the runtime integration must reproduce (exact-match gate).
        if family == "ridge-final" {
            masks.push(Mask::RidgeTrack { drop_bins: 14.0, win: 10, step_max: 4.0, min_peak: 0.12 });
        }
        // Any single ridge config from the command line (pairs with --miss-audit):
        // `--family ridge-one --drop 8 --win 7`.
        if family == "ridge-one" {
            let get = |flag: &str, dflt: f32| -> f32 {
                args.iter()
                    .position(|a| a == flag)
                    .and_then(|i| args.get(i + 1))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(dflt)
            };
            let (drop_bins, win, step_max, min_peak) = (
                get("--drop", 14.0),
                get("--win", 10.0) as usize,
                get("--step", 4.0),
                get("--peak", 0.12),
            );
            let gate = get("--gate", 0.0);
            masks.push(if gate > 0.0 {
                Mask::RidgeGate { drop_bins, win, step_max, min_peak, gate }
            } else {
                Mask::RidgeTrack { drop_bins, win, step_max, min_peak }
            });
        }
        // Round 6: the birth-attack gate over the short-window frontier.
        // Reads recall vs bass-false-fires vs latency per gate step.
        if family == "ridge-gate" {
            for &(win, drop_bins) in &[(6usize, 8.0f32), (7, 8.0), (6, 10.0), (7, 10.0)] {
                for &gate in &[10.0f32, 20.0, 40.0, 80.0] {
                    masks.push(Mask::RidgeGate { drop_bins, win, step_max: 4.0, min_peak: 0.12, gate });
                }
            }
        }
        // Ridge-only threshold placement (pair with `--ridge-only`): finer
        // drop_bins grid, bracketing the kick/bass-portamento line from below.
        // A 120→45 Hz kick clears ~2 bins/hop, so a win-10 window sees ~20 bins;
        // bass portamento (<1 bin/hop) clears <10. drop_bins between those is the
        // knife — this brackets 10→20 to read recall vs bass-fires per step.
        if family == "ridge-sweep" {
            for &drop_bins in &[10.0f32, 12.0, 14.0, 16.0, 18.0, 20.0] {
                for &win in &[8usize, 10, 12] {
                    masks.push(Mask::RidgeTrack { drop_bins, win, step_max: 4.0, min_peak: 0.12 });
                }
            }
        }
        // Latency sweep: the fire lands when the confirmation window FILLS, so
        // `win` is the structural latency (~5.3 ms/hop). Shorter windows fire
        // earlier but see less of the descent, so drop_bins scales with win to
        // stay between the kick's rate (~2 bins/hop over the window) and bass
        // portamento (<1 bin/hop): drop ≈ [1.2×win .. 2×win]. Grades with the
        // signed-offset stats (LATENCY section) to pick the earliest config on
        // the recall/guard plateau.
        if family == "ridge-latency" {
            for &(win, drops) in &[
                (5usize, &[6.0f32, 7.0, 8.0][..]),
                (6, &[7.0, 8.0, 10.0][..]),
                (7, &[8.0, 10.0, 12.0][..]),
                (8, &[9.0, 10.0, 12.0, 14.0][..]),
                (10, &[12.0, 14.0][..]),
            ] {
                for &drop_bins in drops {
                    masks.push(Mask::RidgeTrack { drop_bins, win, step_max: 4.0, min_peak: 0.12 });
                }
            }
        }
    }

    // ── Build clips one at a time; replay every mask over its cached columns.
    //    results[clip_label][mask_idx] = per-band fire hop lists.
    let mut clip_meta: Vec<(String, u32, usize)> = Vec::new(); // label, sr, hop
    let mut results: std::collections::HashMap<String, Vec<[Vec<usize>; 4]>> =
        std::collections::HashMap::new();

    let mut jobs: Vec<(String, Vec<f32>, u32)> = Vec::new();
    for t in TRACKS {
        for s in STEMS {
            let path = format!("{fixtures_root}/{t}/{s}.wav");
            if let Some((mono, sr)) = decode_mono(&path) {
                jobs.push((format!("{t}/{s}"), mono, sr));
            }
        }
    }
    // Guard scenarios (fire-gated selftest subset), same soft_clip as the harness.
    jobs.push(("guard/dive".into(), soft_clip(synth_dive()), SELFTEST_SR));
    jobs.push(("guard/kicks".into(), soft_clip(synth_kicks(Vec::new())), SELFTEST_SR));
    jobs.push(("guard/busymix".into(), soft_clip(synth_kicks(synth_busy_pad())), SELFTEST_SR));
    jobs.push(("guard/densemix".into(), soft_clip(synth_kicks(synth_dense_bed())), SELFTEST_SR));
    jobs.push(("guard/riser".into(), soft_clip(synth_riser()), SELFTEST_SR));
    jobs.push(("guard/growl".into(), soft_clip(synth_growl()), SELFTEST_SR));

    // `--dump <label>` writes a per-hop decision trace for that clip, one CSV
    // per mask, to /tmp/hpss_dump/ (columns per band: candidate, threshold,
    // novelty_ref, is_peak, refractory, fired).
    let dump_label = args
        .iter()
        .position(|a| a == "--dump")
        .and_then(|i| args.get(i + 1).cloned());

    // `--miss-audit`: single-config families only (e.g. ridge-final). Replays
    // the config with per-track lifecycle capture on every labeled clip, then
    // classifies each missed label after the grading section.
    let miss_audit = args.iter().any(|a| a == "--miss-audit");
    let mut ridge_diags: std::collections::HashMap<String, Vec<RidgeDiag>> =
        std::collections::HashMap::new();

    for (label, mono, sr) in &jobs {
        let clip = build_clip(mono, *sr, low_hz, mid_hz);
        let est = Estimates::build(&clip, &masks);
        let per_mask: Vec<[Vec<usize>; 4]> =
            masks.iter().map(|&m| replay_fires(&clip, m, &est)).collect();
        if dump_label.as_deref() == Some(label.as_str()) {
            std::fs::create_dir_all("/tmp/hpss_dump").unwrap();
            for &m in &masks {
                let mut lines = vec![
                    "hop,f_cand,f_thr,f_nov,f_pk,f_refr,f_fired,l_cand,l_thr,l_nov,l_pk,l_refr,l_fired,m_cand,m_thr,m_nov,m_pk,m_refr,m_fired,h_cand,h_thr,h_nov,h_pk,h_refr,h_fired"
                        .to_string(),
                ];
                replay_fires_dump(&clip, m, &est, Some(&mut lines), None);
                let fname = format!("/tmp/hpss_dump/{}.csv", m.name().replace([' ', '='], "_"));
                std::fs::write(&fname, lines.join("\n")).unwrap();
                eprintln!("dumped {fname}");
            }
        }
        if miss_audit
            && masks.len() == 2
            && masks[1].ridge_params().is_some()
            && (label.ends_with("/mix") || label.ends_with("/drums"))
        {
            let mut diag = Vec::new();
            replay_fires_dump(&clip, masks[1], &est, None, Some(&mut diag));
            ridge_diags.insert(label.clone(), diag);
        }
        clip_meta.push((label.clone(), *sr, clip.hop));
        results.insert(label.clone(), per_mask);
        eprintln!("done {label} ({} hops)", clip.n_hops);
    }

    // ── Validation table: baseline (mask-off) fire counts per band, ALL hops —
    //    must exactly match mod_harness's CSV fire counts per clip. ──────────
    println!("\n== VALIDATION (baseline fire counts, all hops: full/low/mid/high) ==");
    for (label, _, _) in &clip_meta {
        let f = &results[label][0];
        println!("{label}: {} {} {} {}", f[0].len(), f[1].len(), f[2].len(), f[3].len());
    }
    if validate_only {
        return;
    }
    // Single-config runs (e.g. --family or-final): print that config's full
    // per-band fire counts in the same format — the integration's reference.
    if masks.len() == 2 {
        println!("\n== REFERENCE ({}, fire counts, all hops: full/low/mid/high) ==", masks[1].name());
        for (label, _, _) in &clip_meta {
            let f = &results[label][1];
            println!("{label}: {} {} {} {}", f[0].len(), f[1].len(), f[2].len(), f[3].len());
        }
    }

    // ── Ground truth per track: BASELINE drums-stem fires ───────────────────
    let hop_s = |label: &str, i: usize| -> f32 {
        let (_, sr, hop) = clip_meta.iter().find(|(l, _, _)| l == label).unwrap();
        i as f32 * *hop as f32 / *sr as f32
    };
    let times = |label: &str, mask_idx: usize, band: usize| -> Vec<f32> {
        results[label][mask_idx][band]
            .iter()
            .filter(|&&i| i >= WARMUP_HOPS)
            .map(|&i| hop_s(label, i))
            .collect()
    };

    println!("\n== SWEEP ==");
    println!(
        "guards gate: dive F=0,L=0 | riser F=0 | growl F=0 | kicks L==8 | busymix L>=7 | densemix L>=6"
    );
    for (mi, mask) in masks.iter().enumerate() {
        // Guards.
        let g = |name: &str, band: usize| -> usize {
            count_post_warmup(&results[&format!("guard/{name}")][mi][band])
        };
        let (dive_f, dive_l) = (g("dive", 0), g("dive", 1));
        let (riser_f, growl_f) = (g("riser", 0), g("growl", 0));
        let (kicks_l, busy_l, dense_l) = (g("kicks", 1), g("busymix", 1), g("densemix", 1));
        // The real harness gates (print_p3_fires) + spam ceilings: the >= gates
        // alone would bless a config that fires 18× in busymix (10 false). The
        // scenarios contain exactly 8 kicks; baseline catches busymix 8,
        // densemix 6. dive-Low's 2 baseline fires are ungated but must not grow.
        let guards_ok = dive_f == 0
            && dive_l <= 2
            && riser_f == 0
            && growl_f == 0
            && kicks_l == 8
            && (7..=8).contains(&busy_l)
            && (6..=8).contains(&dense_l);

        // Per-track recovery / spurious (mix-Low vs BASELINE drums-Low).
        let mut cells: Vec<String> = Vec::new();
        let mut min_drums_retention = f32::INFINITY;
        for t in TRACKS {
            let mix = format!("{t}/mix");
            let drums = format!("{t}/drums");
            let gt = times(&drums, 0, 1); // baseline drums Low = ground truth
            let mut alibi = times(&drums, 0, 0); // baseline drums Full ∪ Low
            alibi.extend_from_slice(&gt);
            let mix_low = times(&mix, mi, 1);
            let recovered = gt.iter().filter(|&&g| matched_within(g, &mix_low, 0.035)).count();
            // Second tolerance: low-band VQT kernels smear a mix kick's rise
            // vs the dry stem's, so a fire can land a few hops late — ±70 ms
            // separates "missing" from "late".
            let rec70 = gt.iter().filter(|&&g| matched_within(g, &mix_low, 0.070)).count();
            let spurious =
                mix_low.iter().filter(|&&m| !matched_within(m, &alibi, 0.050)).count();
            cells.push(format!(
                "{}:{}({})/{} sp{}",
                &t[..4.min(t.len())],
                recovered,
                rec70,
                gt.len(),
                spurious
            ));
            let dr_base = times(&drums, 0, 1).len();
            let dr_cfg = times(&drums, mi, 1).len();
            if dr_base > 0 {
                min_drums_retention = min_drums_retention.min(dr_cfg as f32 / dr_base as f32);
            }
        }
        println!(
            "{:14} | guards {} (dF{dive_f} dL{dive_l} rF{riser_f} gF{growl_f} k{kicks_l} b{busy_l} d{dense_l}) | {} | drums-ret {:.2}",
            mask.name(),
            if guards_ok { "PASS" } else { "FAIL" },
            cells.join("  "),
            min_drums_retention
        );
    }

    // ── LABELS: grade mix-Low and drums-Low fires against the 73 hand-verified
    //    kick labels (README provenance), in seconds, mix and stems separately.
    //    This replaces the circular "baseline drums fires as GT" grading above.
    //    Per mask: total recall@±35ms / @±70ms / labels, and spurious fires. ──
    let labels: std::collections::HashMap<&str, (Vec<f32>, Vec<f32>)> =
        TRACKS.iter().map(|&t| (t, load_labels(t))).collect();
    println!("\n== LABELS (mix-Low vs mix_time_s | drums-Low vs drums_time_s; r35/r70/N sp) ==");
    for (mi, mask) in masks.iter().enumerate() {
        let g = |name: &str, band: usize| -> usize {
            count_post_warmup(&results[&format!("guard/{name}")][mi][band])
        };
        let guards_ok = g("dive", 0) == 0
            && g("dive", 1) <= 2
            && g("riser", 0) == 0
            && g("growl", 0) == 0
            && g("kicks", 1) == 8
            && (7..=8).contains(&g("busymix", 1))
            && (6..=8).contains(&g("densemix", 1));
        let kicks_l = g("kicks", 1);
        let (mut mr35, mut mr70, mut msp, mut mn) = (0, 0, 0, 0);
        let (mut dr35, mut dr70, mut dsp, mut dn) = (0, 0, 0, 0);
        let mut mbass = 0; // mix fires near NO kick label AND NO drums-stem onset
        let (mut mlat, mut dlat): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
        let mut cells: Vec<String> = Vec::new();
        for t in TRACKS {
            let (ml, dl) = &labels[t];
            let mix_low = times(&format!("{t}/mix"), mi, 1);
            let drums_low = times(&format!("{t}/drums"), mi, 1);
            mlat.extend(latencies_ms(&mix_low, ml));
            dlat.extend(latencies_ms(&drums_low, dl));
            // Alibi for "a drum was here": the isolated drums stem's BASELINE
            // (mask-off) Low fires — approximate, but the only per-track drum-
            // activity oracle we have beyond the kick-only labels.
            let drum_alibi = times(&format!("{t}/drums"), 0, 1);
            let (a35, a70, asp) = grade(&mix_low, ml);
            let (b35, b70, bsp) = grade(&drums_low, dl);
            let bass = mix_low
                .iter()
                .filter(|&&f| !matched_within(f, ml, 0.070) && !matched_within(f, &drum_alibi, 0.070))
                .count();
            mr35 += a35;
            mr70 += a70;
            msp += asp;
            mn += ml.len();
            dr35 += b35;
            dr70 += b70;
            dsp += bsp;
            dn += dl.len();
            mbass += bass;
            cells.push(format!(
                "{}:m{a35}({a70})/{} sp{asp} bass{bass}",
                &t[..4.min(t.len())],
                ml.len()
            ));
        }
        // Spurious anatomy: is a mix false fire an ECHO of a real kick (a second
        // track confirming 70-160 ms after the label — refractory-fixable), or
        // an event on another stem (bass-note / non-kick-drum onset), or truly
        // unexplained? Decides WHICH lever claws precision back.
        let (mut echo, mut nbass, mut ndrum, mut nother, mut unexpl) = (0, 0, 0, 0, 0);
        for t in TRACKS {
            let (ml, _) = &labels[t];
            let mix_low = times(&format!("{t}/mix"), mi, 1);
            let bass_on = times(&format!("{t}/bass"), 0, 1);
            let drum_on = times(&format!("{t}/drums"), 0, 1);
            let other_on = times(&format!("{t}/others"), 0, 1);
            for &f in mix_low.iter().filter(|&&f| !matched_within(f, ml, 0.070)) {
                if ml.iter().any(|&l| (0.070..0.160).contains(&(f - l))) {
                    echo += 1;
                } else if matched_within(f, &bass_on, 0.070) {
                    nbass += 1;
                } else if matched_within(f, &drum_on, 0.070) {
                    ndrum += 1;
                } else if matched_within(f, &other_on, 0.070) {
                    nother += 1;
                } else {
                    unexpl += 1;
                }
            }
        }
        let lat = |v: Vec<f32>| {
            latency_stats(v)
                .map_or("lat -/-".into(), |(m, p)| format!("lat {m:+.0}/{p:+.0}ms"))
        };
        println!(
            "{:22} | guards {} k{kicks_l} | MIX {mr35}({mr70})/{mn} sp{msp} bass{mbass} {} | DRUMS {dr35}({dr70})/{dn} sp{dsp} {} | sp: echo{echo} bass{nbass} drum{ndrum} oth{nother} ?{unexpl}",
            mask.name(),
            if guards_ok { "PASS" } else { "FAIL" },
            lat(mlat),
            lat(dlat),
        );
        println!("    {}", cells.join("  "));
    }

    // ── Retention detail: per-clip Low fire counts, baseline vs each config —
    //    read this for the winner before proposing constants. ────────────────
    println!("\n== RETENTION (Low-band fires per clip, baseline first) ==");
    for (label, _, _) in clip_meta.iter().filter(|(l, _, _)| !l.starts_with("guard/")) {
        let row: Vec<String> = masks
            .iter()
            .enumerate()
            .map(|(mi, _)| format!("{}", times(label, mi, 1).len()))
            .collect();
        println!("{label}: {}", row.join(" "));
    }
    println!("\ncolumns: {}", masks.iter().map(|m| m.name()).collect::<Vec<_>>().join(" | "));

    // ── MISS AUDIT: classify every label the single ridge config missed ──────
    if miss_audit && !ridge_diags.is_empty() {
        let (drop_bins, win, _, _, gate) = masks[1].ridge_params().unwrap();
        println!("\n== MISS AUDIT ({}, missed labels @±70ms) ==", masks[1].name());
        let mut buckets: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for t in TRACKS {
            let (ml, dl) = &labels[t];
            for (stem, lab) in [("mix", ml), ("drums", dl)] {
                let clip_label = format!("{t}/{stem}");
                let Some(diags) = ridge_diags.get(&clip_label) else { continue };
                let fires = times(&clip_label, 1, 1);
                let (_, sr, hop) =
                    clip_meta.iter().find(|(l, _, _)| l == &clip_label).unwrap();
                for &l in lab {
                    if matched_within(l, &fires, 0.070) {
                        continue;
                    }
                    let hop_l = l * *sr as f32 / *hop as f32;
                    let class = classify_miss(hop_l, diags, drop_bins, win, gate);
                    *buckets.entry(class.split(' ').next().unwrap().into()).or_default() += 1;
                    println!("{clip_label} @{l:.3}s: {class}");
                }
            }
        }
        let mut counts: Vec<_> = buckets.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        println!(
            "buckets ({total} misses): {}",
            counts.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ")
        );
    }
}
