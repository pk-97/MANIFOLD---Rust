//! The trailing 2048-sample Hann spectrum and its per-hop scalars.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub(super) const FFT_SIZE: usize = 2048;
/// rfft bins from 30 Hz to 8 kHz at 48 kHz (recipe version 1).
pub(super) const BINS: usize = 340;
pub(super) const SPLIT_BANDS: usize = 3;

/// One hop's spectral scalars: whole-range relative flux and log2 centroid, then the same per
/// bandwise band (low, body, upper).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SpecRow {
    pub rel_flux: f64,
    pub centroid: f64,
    pub band_rel_flux: [f64; SPLIT_BANDS],
    pub band_centroid: [f64; SPLIT_BANDS],
}

/// Ports `kick_fusion_features._spectra` (with `kick_tonal_experiment._trailing_frames`) and the
/// flux/centroid reductions of `fusion_features` and `kick_fusion_bandwise._bandwise_features`.
/// Frame k is the 2048 samples ending at (k + 1) * hop, zero-padded before the stream start; the
/// ring starts zeroed, which is that padding.
pub(super) struct TrailingSpectrum {
    ring: Box<[f64; FFT_SIZE]>,
    pos: usize,
    window: Box<[f64; FFT_SIZE]>,
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    first_bin: usize,
    log2_freq: Box<[f64; BINS]>,
    splits: [usize; SPLIT_BANDS + 1],
    eps: f64,
    cur: Box<[f64; BINS]>,
    prev: Box<[f64; BINS]>,
    rise: Box<[f64; BINS]>,
    weighted: Box<[f64; BINS]>,
    have_prev: bool,
}

impl TrailingSpectrum {
    pub(super) fn new(window: &[f64], first_bin: usize, log2_freq: &[f64], splits: [usize; SPLIT_BANDS + 1], eps: f64) -> Self {
        let fft = FftPlanner::<f64>::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            ring: Box::new([0.0; FFT_SIZE]),
            pos: 0,
            window: Box::new(window.try_into().expect("window length checked at load")),
            fft,
            buf: vec![Complex::default(); FFT_SIZE],
            scratch,
            first_bin,
            log2_freq: Box::new(log2_freq.try_into().expect("bin count checked at load")),
            splits,
            eps,
            cur: Box::new([0.0; BINS]),
            prev: Box::new([0.0; BINS]),
            rise: Box::new([0.0; BINS]),
            weighted: Box::new([0.0; BINS]),
            have_prev: false,
        }
    }

    #[inline]
    pub(super) fn push(&mut self, x: f64) {
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % FFT_SIZE;
    }

    /// The scalars of the frame ending at the last pushed sample. Call once per completed hop.
    pub(super) fn hop(&mut self) -> SpecRow {
        let (old, new) = self.ring.split_at(self.pos);
        for ((c, &x), &w) in self.buf.iter_mut().zip(new.iter().chain(old)).zip(self.window.iter()) {
            *c = Complex::new(x * w, 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        for (m, c) in self.cur.iter_mut().zip(&self.buf[self.first_bin..]) {
            *m = c.norm();
        }
        for i in 0..BINS {
            // np.maximum(spectra[1:] - spectra[:-1], 0.0)
            self.rise[i] = (self.cur[i] - self.prev[i]).max(0.0);
            self.weighted[i] = self.cur[i] * self.log2_freq[i];
        }
        let first = !self.have_prev;
        let eps = self.eps;
        let reduce = |r: std::ops::Range<usize>, cur: &[f64], rise: &[f64], weighted: &[f64]| {
            let sum = np_sum(&cur[r.clone()]);
            // Hop 0 has no previous frame: its flux is the frame's own sum.
            let flux = if first { sum } else { np_sum(&rise[r.clone()]) };
            (flux / (sum + eps), np_sum(&weighted[r]) / (sum + eps))
        };
        let (rel_flux, centroid) = reduce(0..BINS, &self.cur[..], &self.rise[..], &self.weighted[..]);
        let mut row = SpecRow { rel_flux, centroid, ..SpecRow::default() };
        for b in 0..SPLIT_BANDS {
            let (f, c) = reduce(self.splits[b]..self.splits[b + 1], &self.cur[..], &self.rise[..], &self.weighted[..]);
            row.band_rel_flux[b] = f;
            row.band_centroid[b] = c;
        }
        std::mem::swap(&mut self.cur, &mut self.prev);
        self.have_prev = true;
        row
    }
}

/// `np.sum` of a contiguous f64 slice: numpy's pairwise summation (`loops_utils.h.src`
/// `pairwise_sum`), added to the reduction identity 0.0. Kept so sums match numpy's rounding.
pub(super) fn np_sum(a: &[f64]) -> f64 {
    0.0 + pairwise(a)
}

fn pairwise(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut res = -0.0;
        for &v in a {
            res += v;
        }
        res
    } else if n <= 128 {
        let mut r = [0.0; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - n % 8 {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for &v in &a[i..] {
            res += v;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise(&a[..n2]) + pairwise(&a[n2..])
    }
}
