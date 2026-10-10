//! The causal 32-band log spectrum the templates and rise profile read.
//! Port of `band_spec` in `tools/audio_analysis/eval/kick_goal_templates.py`, plus the
//! 16 paired bands of `rise_profile` in `kick_goal_profile.py`.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::dsp::History;
use super::HIST_FRAMES;
use crate::kick::container::{Container, ContainerError};

pub(super) const N_FFT: usize = 1024;
pub(super) const BANDS: usize = 32;
pub(super) const PAIRS: usize = BANDS / 2;

pub(super) struct BandSpec {
    hop: u64,
    window: Vec<f64>,
    /// Band of each rfft bin, `-1` for bins outside 30 Hz–8 kHz.
    band_of_bin: Vec<i64>,
    floor: f64,
    /// The last `N_FFT` samples, zero before the stream (the reference zero-pads).
    ring: Vec<f64>,
    pos: usize,
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    frames: History<[f64; BANDS]>,
    paired: History<[f64; PAIRS]>,
}

impl BandSpec {
    pub fn new(c: &Container, hop: u64) -> Result<Self, ContainerError> {
        let window = c.f64s("extra.spec_window", &[N_FFT])?.to_vec();
        let band_of_bin = c.i64s("extra.spec_band_of_bin", &[N_FFT / 2 + 1])?.to_vec();
        if band_of_bin.iter().any(|&b| b < -1 || b >= BANDS as i64) {
            return Err(ContainerError::WrongType { name: "extra.spec_band_of_bin".into(), want: "band index in -1..32" });
        }
        let fft = FftPlanner::new().plan_fft_forward(N_FFT);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        Ok(Self {
            hop,
            window,
            band_of_bin,
            floor: super::scalar(c, "extra.spec_floor")?,
            ring: vec![0.0; N_FFT],
            pos: 0,
            fft,
            buf: vec![Complex::new(0.0, 0.0); N_FFT],
            scratch,
            frames: History::new(HIST_FRAMES),
            paired: History::new(HIST_FRAMES),
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

    /// Frame `k` covers the `N_FFT` samples ending at `(k + 1) * hop`.
    fn frame(&mut self) {
        for (i, b) in self.buf.iter_mut().enumerate() {
            *b = Complex::new(self.ring[(self.pos + i) % N_FFT] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        let mut power = [0.0; BANDS];
        for (bin, &band) in self.band_of_bin.iter().enumerate() {
            if band >= 0 {
                let a = self.buf[bin].norm();
                power[band as usize] += a * a;
            }
        }
        let spec = power.map(|p| (p + self.floor).log10());
        let mut paired = [0.0; PAIRS];
        for (b, p) in paired.iter_mut().enumerate() {
            *p = 10.0 * (0.5 * (10f64.powf(spec[2 * b]) + 10f64.powf(spec[2 * b + 1]))).log10();
        }
        self.frames.push(spec);
        self.paired.push(paired);
    }


    pub fn holds(&self, first: u64, last: u64) -> bool {
        self.frames.holds(first, last)
    }

    pub fn frame_at(&self, k: u64) -> &[f64; BANDS] {
        self.frames.get(k)
    }

    pub fn paired_at(&self, k: u64) -> &[f64; PAIRS] {
        self.paired.get(k)
    }
}
