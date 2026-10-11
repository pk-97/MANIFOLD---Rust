//! The tonal-onset set (note13): low pitch before and after the onset (YIN on a causal 4 kHz series), pitch
//! change, YIN dips, low levels, chroma change and entropy, mid-band level change and the share of new
//! spectral peaks. A pitched stab, bass note or voice reads tonal; a snare or clap reads as noise.
//! Port of `pitch_features` / `yin` / `chroma` in `tools/audio_analysis/eval/note_pitch.py`.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::dsp::{History, Sos, np_sum};
use crate::trained::container::{Container, ContainerError};

pub(super) const N_FFT: usize = 2048;
const BINS: usize = N_FFT / 2 + 1;
/// Seconds of raw audio and of the 4 kHz series kept (decisions come well within this of their data).
const X_HISTORY: usize = 1 << 18;
const LOW_HISTORY: usize = 1 << 15;
/// `yin` defaults: 40–400 Hz, threshold .15, a dip of .5 or more is unpitched.
const FMIN: f64 = 40.0;
const FMAX: f64 = 400.0;
const YIN_THRESH: f64 = 0.15;
const YIN_PITCHED: f64 = 0.5;
/// Low windows: 70 to 10 ms before the onset, and the 40 ms ending at the emission.
const BEFORE: (f64, f64) = (0.070, 0.010);
const AFTER_S: f64 = 0.040;
/// The before-spectrum ends 10 ms before the onset.
const SPEC_BEFORE_S: f64 = 0.010;
const STRONG_DB: f64 = 30.0;
const NEW_PEAK_DB: f64 = 6.0;
const LEVEL_FLOOR: f64 = 1e-12;
const EMPTY_LEVEL_DB: f64 = -120.0;
const MAG_FLOOR: f64 = 1e-9;
const CHROMA_FLOOR: f64 = 1e-12;

pub(super) struct NotePitch {
    sr: f64,
    low_rate: f64,
    decim: u64,
    lp: Sos,
    x: History<f64>,
    low: History<f64>,
    window: Vec<f64>,
    mid: (usize, usize),
    chroma: (usize, usize),
    chroma_class: Vec<usize>,
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    seg: Vec<f64>,
    diff: Vec<f64>,
    d: Vec<f64>,
    cmnd: Vec<f64>,
    mag_before: Vec<f64>,
    mag_after: Vec<f64>,
    db_before: Vec<f64>,
    db_after: Vec<f64>,
    tmp: Vec<f64>,
}

impl NotePitch {
    pub fn new(c: &Container, sr: f64) -> Result<Self, ContainerError> {
        let low_rate = super::scalar_i64(c, "extra.note_low_rate")?;
        if low_rate <= 0 || (sr as i64) % low_rate != 0 {
            return Err(ContainerError::WrongType { name: "extra.note_low_rate".into(), want: "a divisor of the model rate" });
        }
        let bins = |name: &str| -> Result<(usize, usize), ContainerError> {
            let v = c.i64s(name, &[2])?;
            if v[0] < 0 || v[1] <= 0 || (v[0] + v[1]) as usize > BINS {
                return Err(ContainerError::WrongType { name: name.into(), want: "bins inside the rfft" });
            }
            Ok((v[0] as usize, v[1] as usize))
        };
        let chroma = bins("extra.note_chroma_bins")?;
        let class = c.i64s("extra.note_chroma_class", &[chroma.1])?;
        if class.iter().any(|&k| !(0..12).contains(&k)) {
            return Err(ContainerError::WrongType { name: "extra.note_chroma_class".into(), want: "pitch classes 0..12" });
        }
        let fft = FftPlanner::new().plan_fft_forward(N_FFT);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        let hi = (low_rate as f64 / FMIN) as usize;
        Ok(Self {
            sr,
            low_rate: low_rate as f64,
            decim: sr as u64 / low_rate as u64,
            lp: Sos::new(c.f64s("extra.note_lp_sos", &[3, 6])?),
            x: History::new(X_HISTORY),
            low: History::new(LOW_HISTORY),
            window: c.f64s("extra.note_window", &[N_FFT])?.to_vec(),
            mid: bins("extra.note_mid_bins")?,
            chroma,
            chroma_class: class.iter().map(|&k| k as usize).collect(),
            fft,
            buf: vec![Complex::new(0.0, 0.0); N_FFT],
            scratch,
            seg: Vec::with_capacity(LOW_HISTORY),
            diff: Vec::with_capacity(LOW_HISTORY),
            d: vec![0.0; hi + 1],
            cmnd: vec![0.0; hi + 1],
            mag_before: vec![0.0; BINS],
            mag_after: vec![0.0; BINS],
            db_before: vec![0.0; BINS],
            db_after: vec![0.0; BINS],
            tmp: vec![0.0; BINS],
        })
    }

    /// Sample `s`: the raw ring, and the low-passed series read at each 4 kHz tick (`lp[(k * sr) // rate]`).
    #[inline]
    pub fn push(&mut self, s: u64, x: f64) {
        self.x.push(x);
        let y = self.lp.step(x);
        if s.is_multiple_of(self.decim) {
            self.low.push(y);
        }
    }

    /// Everything up to the emission hop's end.
    pub fn ready_sample(&self, a: u64, hop: u64) -> u64 {
        (a + 1) * hop
    }

    /// `low[max(0, int(a * rate)):max(0, int(b * rate))]` into `self.seg`.
    fn low_seg(&mut self, a: f64, b: f64) {
        let lo = ((a * self.low_rate) as i64).max(0) as u64;
        let hi = ((b * self.low_rate) as i64).max(0) as u64;
        self.seg.clear();
        for k in lo..hi.min(self.low.len()) {
            self.seg.push(*self.low.get(k));
        }
    }

    /// `yin` on `self.seg`: (f0 Hz or 0, dip).
    fn yin(&mut self) -> (f64, f64) {
        let (lo, hi) = ((self.low_rate / FMAX) as usize, (self.low_rate / FMIN) as usize);
        let n = self.seg.len();
        if n < hi + lo {
            return (0.0, 1.0);
        }
        let w = n - hi;
        for k in 0..=hi {
            self.diff.clear();
            self.diff.extend((0..w).map(|i| (self.seg[i] - self.seg[k + i]).powi(2)));
            self.d[k] = np_sum(&self.diff);
        }
        self.cmnd[0] = 1.0;
        let mut cum = 0.0;
        for k in 1..=hi {
            cum += self.d[k];
            self.cmnd[k] = self.d[k] * k as f64 / cum.max(1e-12);
        }
        let band = &self.cmnd[lo..=hi];
        let mut k = match band.iter().position(|&v| v < YIN_THRESH) {
            Some(k) => k,
            // np.argmin: the first minimum.
            None => band.iter().enumerate().fold(0, |m, (i, &v)| if v < band[m] { i } else { m }),
        };
        while k + 1 < band.len() && band[k + 1] < band[k] {
            k += 1;
        }
        let dip = band[k];
        if dip < YIN_PITCHED { (self.low_rate / (lo + k) as f64, dip) } else { (0.0, dip) }
    }

    /// `10 log10(mean(seg^2) + 1e-12)`, or -120 for an empty segment.
    fn level(&mut self) -> f64 {
        if self.seg.is_empty() {
            return EMPTY_LEVEL_DB;
        }
        self.diff.clear();
        self.diff.extend(self.seg.iter().map(|v| v * v));
        10.0 * (np_sum(&self.diff) / self.seg.len() as f64 + LEVEL_FLOOR).log10()
    }

    /// `|rfft(hanning * x[e - N_FFT:e])|`, zero before the stream, `e = max(0, int(t * sr))`.
    fn spectrum_ending(&mut self, t: f64, after: bool) {
        let e = ((t * self.sr) as i64).max(0) as u64;
        for (i, b) in self.buf.iter_mut().enumerate() {
            let s = (e + i as u64).checked_sub(N_FFT as u64);
            let v = s.map_or(0.0, |s| *self.x.get(s));
            *b = Complex::new(v * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        let out = if after { &mut self.mag_after } else { &mut self.mag_before };
        for (o, b) in out.iter_mut().zip(&self.buf) {
            *o = b.norm();
        }
    }

    /// `chroma`: pitch-class weights of the magnitudes, normalised to sum 1.
    fn chroma(&self, mag: &[f64]) -> [f64; 12] {
        let mut c = [0.0; 12];
        let (lo, n) = self.chroma;
        for (i, &class) in self.chroma_class.iter().enumerate().take(n) {
            c[class] += mag[lo + i];
        }
        let total = np_sum(&c).max(CHROMA_FLOOR);
        c.map(|v| v / total)
    }

    /// The 13 numbers for a candidate with onset time `on` and emission time `em` (seconds).
    pub fn features(&mut self, on: f64, em: f64, out: &mut [f64]) {
        self.low_seg(on - BEFORE.0, on - BEFORE.1);
        let (f0b, db) = if self.seg.is_empty() { (0.0, 1.0) } else { self.yin() };
        let lb = self.level();
        self.low_seg(em - AFTER_S, em);
        let (f0a, da) = if self.seg.is_empty() { (0.0, 1.0) } else { self.yin() };
        let la = self.level();
        let st = if f0a != 0.0 && f0b != 0.0 { (12.0 * (f0a / f0b).log2()).abs() } else { 0.0 };
        self.spectrum_ending(on - SPEC_BEFORE_S, false);
        self.spectrum_ending(em, true);
        let cb = self.chroma(&self.mag_before);
        let ca = self.chroma(&self.mag_after);
        let dot: f64 = cb.iter().zip(&ca).map(|(a, b)| a * b).sum();
        let norm = |v: &[f64; 12]| v.iter().map(|a| a * a).sum::<f64>().sqrt();
        let cos = 1.0 - dot / (norm(&cb) * norm(&ca)).max(CHROMA_FLOOR);
        let ent = -np_sum(&ca.map(|p| p * (p + CHROMA_FLOOR).ln()));
        let rise = ca.iter().zip(&cb).map(|(a, b)| a - b).fold(f64::NEG_INFINITY, f64::max);
        let (lo, n) = self.mid;
        let tmp = &mut self.tmp[..n];
        for (t, m) in tmp.iter_mut().zip(&self.mag_before[lo..lo + n]) {
            *t = m * m;
        }
        let mb = np_sum(tmp);
        for (t, m) in tmp.iter_mut().zip(&self.mag_after[lo..lo + n]) {
            *t = m * m;
        }
        let ma = np_sum(tmp);
        for i in 0..n {
            self.db_before[i] = 20.0 * (self.mag_before[lo + i] + MAG_FLOOR).log10();
            self.db_after[i] = 20.0 * (self.mag_after[lo + i] + MAG_FLOOR).log10();
        }
        let (lb_, la_) = (&self.db_before[..n], &self.db_after[..n]);
        let top = la_.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // np.roll wraps: the first bin's left neighbour is the last bin and the reverse.
        let (mut peaks, mut new) = (0usize, 0usize);
        for i in 0..n {
            let (left, right) = (la_[(i + n - 1) % n], la_[(i + 1) % n]);
            if la_[i] >= top - STRONG_DB && la_[i] >= left && la_[i] >= right {
                peaks += 1;
                if la_[i] - lb_[i] >= NEW_PEAK_DB {
                    new += 1;
                }
            }
        }
        let new_share = if peaks > 0 { new as f64 / peaks as f64 } else { 0.0 };
        let l2 = |f: f64| if f != 0.0 { f.log2() } else { 0.0 };
        out.copy_from_slice(&[
            l2(f0b),
            l2(f0a),
            st,
            db,
            da,
            lb,
            la,
            la - lb,
            cos,
            rise,
            ent,
            10.0 * ((ma + LEVEL_FLOOR) / (mb + LEVEL_FLOOR)).log10(),
            new_share,
        ]);
    }
}
