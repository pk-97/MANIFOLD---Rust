//! Complex-domain prediction error (cdom4): how far each 45 Hz–2 kHz bin strays from the steady-state
//! prediction of its last two spectra. A held tone is predictable, a noise burst is not.
//! Port of `complex_features` in `tools/audio_analysis/eval/kick_complex_onset.py` and `_cdom4` in
//! `tools/audio_analysis/lab/components.py`.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::dsp::History;
use super::HIST_FRAMES;
use crate::trained::container::{Container, ContainerError};

pub(super) const N_FFT: usize = 2048;
/// Hops before the onset the pre-onset means read (`_cdom4`).
const PRE_HOPS: u64 = 10;

pub(super) struct ComplexOnset {
    hop: u64,
    window: Vec<f64>,
    window_sum: f64,
    bin_lo: usize,
    bin_n: usize,
    floor: f64,
    raw_eps: f64,
    /// The last `N_FFT` samples, zero before the stream (the reference zero-pads).
    ring: Vec<f64>,
    pos: usize,
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    prev: Vec<Complex<f64>>,
    prev2: Vec<Complex<f64>>,
    /// Per hop: (normalised error, log10(raw error + eps)).
    hops: History<[f64; 2]>,
}

impl ComplexOnset {
    pub fn new(c: &Container, hop: u64) -> Result<Self, ContainerError> {
        let window = c.f64s("extra.cdom_window", &[N_FFT])?.to_vec();
        let lo = super::scalar_i64(c, "extra.cdom_bin_lo")?;
        let n = super::scalar_i64(c, "extra.cdom_bin_n")?;
        if lo < 0 || n <= 0 || (lo + n) as usize > N_FFT / 2 + 1 {
            return Err(ContainerError::WrongType { name: "extra.cdom_bin_lo/extra.cdom_bin_n".into(), want: "bins inside the rfft" });
        }
        let (bin_lo, bin_n) = (lo as usize, n as usize);
        let fft = FftPlanner::new().plan_fft_forward(N_FFT);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        Ok(Self {
            hop,
            window_sum: window.iter().sum(),
            window,
            bin_lo,
            bin_n,
            floor: super::scalar(c, "extra.cdom_floor")?,
            raw_eps: super::scalar(c, "extra.cdom_raw_eps")?,
            ring: vec![0.0; N_FFT],
            pos: 0,
            fft,
            buf: vec![Complex::new(0.0, 0.0); N_FFT],
            scratch,
            prev: vec![Complex::new(0.0, 0.0); bin_n],
            prev2: vec![Complex::new(0.0, 0.0); bin_n],
            hops: History::new(HIST_FRAMES),
        })
    }

    #[inline]
    pub fn push(&mut self, s: u64, x: f64) {
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % N_FFT;
        if (s + 1).is_multiple_of(self.hop) {
            self.frame();
        }
    }

    /// Hop `k`'s frame covers the `N_FFT` samples ending at `(k + 1) * hop`; hops 0 and 1 read 0 (no
    /// prediction yet).
    fn frame(&mut self) {
        for (i, b) in self.buf.iter_mut().enumerate() {
            *b = Complex::new(self.ring[(self.pos + i) % N_FFT] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        let cur = &self.buf[self.bin_lo..self.bin_lo + self.bin_n];
        let (mut nrm, mut raw) = (0.0, 0.0);
        if self.hops.len() >= 2 {
            let (mut err, mut den) = (0.0, 0.0);
            for ((x, p), q) in cur.iter().zip(&self.prev).zip(&self.prev2) {
                let predicted = Complex::from_polar(p.norm(), 2.0 * p.arg() - q.arg());
                err += (x - predicted).norm();
                den += x.norm() + p.norm();
            }
            raw = err / self.window_sum;
            if den / self.window_sum > self.floor {
                nrm = (err / den).clamp(0.0, 1.0);
            }
        }
        self.hops.push([nrm, (raw + self.raw_eps).log10()]);
        std::mem::swap(&mut self.prev, &mut self.prev2);
        self.prev.copy_from_slice(cur);
    }

    /// Hop `a` (the emission) must be in.
    pub fn ready_sample(&self, a: u64) -> u64 {
        (a + 1) * self.hop
    }

    /// Max normalised and max log raw error from onset hop `c` to emission hop `a`, then their means over the
    /// `PRE_HOPS` hops before the onset (0 and the onset's log raw error when there are none).
    pub fn features(&self, c: u64, a: u64, out: &mut [f64]) {
        let (mut max_n, mut max_r) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for h in c..=a {
            let [n, r] = *self.hops.get(h);
            max_n = max_n.max(n);
            max_r = max_r.max(r);
        }
        let lo = c.saturating_sub(PRE_HOPS);
        let (mean_n, mean_r) = if c > lo {
            let (mut sn, mut sr) = (0.0, 0.0);
            for h in lo..c {
                let [n, r] = *self.hops.get(h);
                sn += n;
                sr += r;
            }
            let k = (c - lo) as f64;
            (sn / k, sr / k)
        } else {
            (0.0, self.hops.get(c)[1])
        };
        out.copy_from_slice(&[max_n, max_r, mean_n, mean_r]);
    }
}
