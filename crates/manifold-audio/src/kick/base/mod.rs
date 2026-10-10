//! Candidates and the 15 base features, streaming (reference:
//! `tools/audio_analysis/eval/kick_fusion_bandwise.py` `fusion_features` over `kick_fusion_features.py`).
//! Every constant comes from the `base.*` model entries (`tools/audio_analysis/kick_export/base.py`).

mod envelopes;
mod spectrum;
#[cfg(test)]
mod tests;

use envelopes::{BandEnvelopes, BODY, FAST, LOW, SLOW, UPPER};
use spectrum::{np_sum, SpecRow, TrailingSpectrum, BINS, FFT_SIZE, SPLIT_BANDS};

use super::container::{Container, ContainerError};

pub const FEATURES: usize = 15;
/// Hops of history kept; a candidate reads hops `cand_hop..=avail_hop`, so `deadline < HISTORY`.
const HISTORY: usize = 16;

/// A kick candidate: the rising-edge hop, the hop whose end makes its features available
/// (`cand_hop + deadline`), and the 15 base features in the reference's column order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub cand_hop: u64,
    pub avail_hop: u64,
    pub features: [f64; FEATURES],
}

#[derive(Debug, Clone, Copy, Default)]
struct HopRow {
    low_fast: f64,
    low_slow: f64,
    body_fast: f64,
    body_slow: f64,
    upper_fast: f64,
    upper_slow: f64,
    spec: SpecRow,
}

struct Params {
    sample_rate: u64,
    hop: usize,
    span: u64,
    deadline: u64,
    eps: f64,
    active_floor: f64,
    low_ratio: f64,
    body_ratio: f64,
    upper_ratio: f64,
    upper_count: usize,
    log_clip: f64,
    lag_norm_s: f64,
}

/// Streaming port of `kick_fusion_bandwise.fusion_features` for a stream that starts at sample 0.
/// Hop k covers samples `[k * hop, (k + 1) * hop)`; its envelopes are read after its last sample and
/// its spectrum is the trailing frame ending there. A candidate is a hop where any of the low, body
/// or upper activity tests turns on; it is emitted when hop `cand_hop + deadline` completes.
pub struct BaseDetector {
    p: Params,
    env: BandEnvelopes,
    spec: TrailingSpectrum,
    phase: usize,
    hop_index: u64,
    rows: [HopRow; HISTORY],
    was_active: [bool; 3],
    pending: [u64; HISTORY],
    pending_head: usize,
    pending_len: usize,
}

fn bad(name: &str, want: u64, got: i64) -> ContainerError {
    ContainerError::WrongShape { name: name.to_owned(), want: vec![want as usize], got: vec![got.max(0) as usize] }
}

fn scalar_f64(c: &Container, name: &str) -> Result<f64, ContainerError> {
    Ok(c.f64s(name, &[])?[0])
}

/// A non-negative i64 scalar no larger than `max` (the bound is reported as `want` on failure).
fn scalar_u64(c: &Container, name: &str, max: u64) -> Result<u64, ContainerError> {
    let v = c.i64s(name, &[])?[0];
    if v < 0 || v as u64 > max {
        return Err(bad(name, max, v));
    }
    Ok(v as u64)
}

impl BaseDetector {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let sample_rate = scalar_u64(c, "base.sample_rate", 48_000)?;
        if sample_rate != 48_000 {
            return Err(bad("base.sample_rate", 48_000, sample_rate as i64));
        }
        let hop = scalar_u64(c, "base.hop", FFT_SIZE as u64)? as usize;
        if hop == 0 {
            return Err(bad("base.hop", 1, 0));
        }
        let deadline = scalar_u64(c, "base.deadline", HISTORY as u64 - 1)?;
        let span = scalar_u64(c, "base.span", deadline)?;
        let bins = c.i64s("base.fft_bins", &[2])?;
        let first_bin = bins[0].max(0) as usize;
        if bins[1] - bins[0] != BINS as i64 || first_bin + BINS > FFT_SIZE / 2 + 1 {
            return Err(bad("base.fft_bins", BINS as u64, bins[1] - bins[0]));
        }
        let s = c.i64s("base.band_splits", &[SPLIT_BANDS + 1])?;
        if s[0] != 0 || s[SPLIT_BANDS] != BINS as i64 || s.windows(2).any(|w| w[0] >= w[1]) {
            return Err(bad("base.band_splits", BINS as u64, s[SPLIT_BANDS]));
        }
        let splits = [s[0] as usize, s[1] as usize, s[2] as usize, s[3] as usize];
        let eps = scalar_f64(c, "base.eps")?;
        let p = Params {
            sample_rate,
            hop,
            span,
            deadline,
            eps,
            active_floor: scalar_f64(c, "base.active_floor")?,
            low_ratio: scalar_f64(c, "base.low_ratio")?,
            body_ratio: scalar_f64(c, "base.body_ratio")?,
            upper_ratio: scalar_f64(c, "base.upper_ratio")?,
            upper_count: scalar_u64(c, "base.upper_count", UPPER.len() as u64)? as usize,
            log_clip: scalar_f64(c, "base.log_clip")?,
            lag_norm_s: scalar_f64(c, "base.lag_norm_s")?,
        };
        let env = BandEnvelopes::new(
            c.f64s("base.band_sos", &[envelopes::BANDS, 2, 6])?,
            c.f64s("base.env_alpha", &[2])?,
            c.f64s("base.env_gain", &[2])?,
        );
        let spec = TrailingSpectrum::new(
            c.f64s("base.fft_window", &[FFT_SIZE])?,
            first_bin,
            c.f64s("base.log2_freq", &[BINS])?,
            splits,
            eps,
        );
        Ok(Self {
            p,
            env,
            spec,
            phase: 0,
            hop_index: 0,
            rows: [HopRow::default(); HISTORY],
            was_active: [false; 3],
            pending: [0; HISTORY],
            pending_head: 0,
            pending_len: 0,
        })
    }

    pub fn hop(&self) -> usize {
        self.p.hop
    }

    /// The most candidates one `push` of `samples` samples can append: one per completed hop.
    pub fn max_candidates(&self, samples: usize) -> usize {
        samples / self.p.hop + 1
    }

    /// The absolute sample count at which the candidate with this `avail_hop` is emitted (and its
    /// features are valid): the end of the availability hop.
    pub fn ready_sample(&self, avail_hop: u64) -> u64 {
        (avail_hop + 1) * self.p.hop as u64
    }

    /// Feeds 48 kHz mono samples. Appends each candidate the moment its availability hop completes.
    /// The caller reserves `max_candidates(samples.len())` spare capacity in `out` first, so `push`
    /// never allocates.
    pub fn push(&mut self, samples: &[f64], out: &mut Vec<Candidate>) {
        debug_assert!(out.capacity() - out.len() >= self.max_candidates(samples.len()), "reserve max_candidates first");
        for &x in samples {
            self.env.step(x);
            self.spec.push(x);
            self.phase += 1;
            if self.phase == self.p.hop {
                self.phase = 0;
                self.complete_hop(out);
                self.hop_index += 1;
            }
        }
    }

    fn row(&self, hop: u64) -> &HopRow {
        &self.rows[(hop % HISTORY as u64) as usize]
    }

    /// `kick_fusion_features._rise_edges` for one hop, then the hop's spectrum, then emission.
    fn complete_hop(&mut self, out: &mut Vec<Candidate>) {
        let p = &self.p;
        let e = *self.env.values();
        // np.sum over the three upper bands (left to right from the identity, as numpy reduces).
        let upper_fast = np_sum(&UPPER.map(|b| e[b][FAST]));
        let upper_slow = np_sum(&UPPER.map(|b| e[b][SLOW]));
        let low_active = e[LOW][FAST] > p.active_floor && e[LOW][FAST] > p.low_ratio * e[LOW][SLOW];
        let body_active = e[BODY][FAST] > p.active_floor && e[BODY][FAST] / (e[BODY][SLOW] + p.eps) > p.body_ratio;
        let rising = UPPER.iter().filter(|&&b| e[b][FAST] / (e[b][SLOW] + p.eps) > p.upper_ratio).count();
        let upper_active = upper_fast > p.active_floor && rising >= p.upper_count;
        let active = [low_active, body_active, upper_active];
        let edge = active.iter().zip(self.was_active).any(|(&now, was)| now && !was);
        self.was_active = active;

        let k = self.hop_index;
        if edge {
            let slot = (self.pending_head + self.pending_len) % HISTORY;
            self.pending[slot] = k;
            self.pending_len += 1;
        }
        let spec = self.spec.hop();
        self.rows[(k % HISTORY as u64) as usize] = HopRow {
            low_fast: e[LOW][FAST],
            low_slow: e[LOW][SLOW],
            body_fast: e[BODY][FAST],
            body_slow: e[BODY][SLOW],
            upper_fast,
            upper_slow,
            spec,
        };
        if self.pending_len > 0 && self.pending[self.pending_head] + self.p.deadline == k {
            let cand = self.pending[self.pending_head];
            self.pending_head = (self.pending_head + 1) % HISTORY;
            self.pending_len -= 1;
            out.push(Candidate { cand_hop: cand, avail_hop: k, features: self.features(cand, k) });
        }
    }

    /// The feature row loop of `kick_fusion_features.fusion_features` and
    /// `kick_fusion_bandwise._bandwise_features` for one candidate.
    fn features(&self, index: u64, deadline: u64) -> [f64; FEATURES] {
        let p = &self.p;
        let eps = p.eps;
        // _window_bounds: inclusive first and last 15 ms windows inside [index, deadline].
        let first = index..=deadline.min(index + p.span);
        let last = index.max(deadline - p.span)..=deadline;
        let whole = index..=deadline;
        let max = |r: std::ops::RangeInclusive<u64>, f: &dyn Fn(&HopRow) -> f64| {
            r.map(|h| f(self.row(h))).fold(f64::NEG_INFINITY, f64::max)
        };
        let mean = |r: std::ops::RangeInclusive<u64>, f: &dyn Fn(&HopRow) -> f64| {
            let mut v = [0.0; HISTORY];
            let mut n = 0;
            for h in r {
                v[n] = f(self.row(h));
                n += 1;
            }
            np_sum(&v[..n]) / n as f64
        };
        // np.argmax: the first maximum, as an offset from `index`.
        let argmax = |f: &dyn Fn(&HopRow) -> f64| {
            let (mut best, mut at) = (f64::NEG_INFINITY, 0u64);
            for h in whole.clone() {
                let v = f(self.row(h));
                if v > best {
                    best = v;
                    at = h - index;
                }
            }
            at
        };
        let clip_log = |v: f64| v.clamp(-p.log_clip, p.log_clip);
        let log_ratio = |a: f64, b: f64| ((a + eps) / (b + eps)).ln();

        let low_rise = max(whole.clone(), &|r| log_ratio(r.low_fast, r.low_slow));
        let body_rise = max(whole.clone(), &|r| log_ratio(r.body_fast, r.body_slow));
        let upper_rise = max(whole.clone(), &|r| log_ratio(r.upper_fast, r.upper_slow));
        let centroid_drop = mean(first.clone(), &|r| r.spec.centroid) - mean(last.clone(), &|r| r.spec.centroid);
        let upper_decay = log_ratio(self.row(deadline).upper_fast, max(first.clone(), &|r| r.upper_fast));
        let balance = log_ratio(max(whole.clone(), &|r| r.body_fast), max(whole.clone(), &|r| r.low_fast));
        let evolution = log_ratio(mean(last.clone(), &|r| r.low_fast), mean(first.clone(), &|r| r.low_fast));
        let lag_hops = argmax(&|r| r.low_fast).abs_diff(argmax(&|r| r.upper_fast));
        let peak_lag = (lag_hops * p.hop as u64) as f64 / p.sample_rate as f64 / p.lag_norm_s;

        let mut f = [0.0; FEATURES];
        f[..9].copy_from_slice(&[
            clip_log(low_rise),
            clip_log(body_rise),
            clip_log(upper_rise),
            max(whole.clone(), &|r| r.spec.rel_flux).clamp(0.0, 1.0),
            centroid_drop.clamp(-p.log_clip, p.log_clip),
            clip_log(upper_decay),
            clip_log(balance),
            clip_log(evolution),
            peak_lag.clamp(0.0, 1.0),
        ]);
        for b in 0..SPLIT_BANDS {
            f[9 + 2 * b] = max(whole.clone(), &|r| r.spec.band_rel_flux[b]).clamp(0.0, 1.0);
            let drop = mean(first.clone(), &|r| r.spec.band_centroid[b]) - mean(last.clone(), &|r| r.spec.band_centroid[b]);
            f[10 + 2 * b] = drop.clamp(-p.log_clip, p.log_clip);
        }
        f
    }
}
