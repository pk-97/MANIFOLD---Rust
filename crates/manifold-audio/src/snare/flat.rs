//! Per-band spectral flatness (noisiness) on the net's 2 ms grid.
//! Reference: `detector_channels.py` `flatness`.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::kick::container::{Container, ContainerError};

pub const BANDS: usize = 64;
/// Frames of history kept, as `kick::net::spectrum::HISTORY`.
pub const HISTORY: usize = 2048;

/// Frame `f` covers the `n_fft` samples ending at sample `f * frame_hop` (exclusive), zeros
/// before the stream; per band `clip(10 log10(geomean(p) / mean(p)))` over its contiguous bins,
/// `p = |rfft(window * frame)|^2 + 1e-12`.
pub struct Flatness {
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    band_lo: [usize; BANDS],
    band_n: [usize; BANDS],
    clip: (f64, f64),
    frame_hop: u64,
    /// The last `n_fft` samples, sample `s` at `s % n_fft`.
    ring: Vec<f64>,
    buf: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    power: Vec<f64>,
    samples: u64,
    frames: u64,
    history: Vec<f32>,
}

impl Flatness {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let n = c.i64s("flat.n_fft", &[])?[0];
        let frame_hop = c.i64s("net.frame_hop", &[])?[0];
        if n <= 0 || frame_hop <= 0 {
            return Err(ContainerError::WrongType { name: "flat.n_fft/net.frame_hop".into(), want: "positive lengths" });
        }
        let n = n as usize;
        let window = c.f64s("flat.window", &[n])?.to_vec();
        let lo = c.i64s("flat.band_lo", &[BANDS])?;
        let cnt = c.i64s("flat.band_n", &[BANDS])?;
        let clip = c.f64s("flat.clip_db", &[2])?;
        let mut band_lo = [0; BANDS];
        let mut band_n = [0; BANDS];
        for b in 0..BANDS {
            if lo[b] < 0 || cnt[b] <= 0 || (lo[b] + cnt[b]) as usize > n / 2 + 1 {
                return Err(ContainerError::WrongType { name: "flat.band_lo/flat.band_n".into(), want: "bins inside the rfft" });
            }
            band_lo[b] = lo[b] as usize;
            band_n[b] = cnt[b] as usize;
        }
        let fft = FftPlanner::new().plan_fft_forward(n);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Ok(Self {
            fft,
            window,
            band_lo,
            band_n,
            clip: (clip[0], clip[1]),
            frame_hop: frame_hop as u64,
            ring: vec![0.0; n],
            buf: vec![Complex::default(); n],
            scratch,
            power: vec![0.0; n / 2 + 1],
            samples: 0,
            frames: 0,
            history: vec![0.0; HISTORY * BANDS],
        })
    }

    /// Frames computed so far; frame `f` exists once sample `f * frame_hop - 1` has been pushed.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn frame_hop(&self) -> u64 {
        self.frame_hop
    }

    /// Frame `f`'s 64 band flatness values in dB; `None` once it has left the history or before it exists.
    pub fn frame(&self, f: u64) -> Option<&[f32]> {
        if f >= self.frames || self.frames - f > HISTORY as u64 {
            return None;
        }
        let at = (f % HISTORY as u64) as usize * BANDS;
        Some(&self.history[at..at + BANDS])
    }

    pub fn push(&mut self, mut x: &[f64]) {
        let n = self.ring.len() as u64;
        loop {
            let end = self.frames * self.frame_hop;
            if self.samples >= end {
                self.emit_frame();
                continue;
            }
            if x.is_empty() {
                return;
            }
            let take = ((end - self.samples) as usize).min(x.len());
            for &v in &x[..take] {
                self.ring[(self.samples % n) as usize] = v;
                self.samples += 1;
            }
            x = &x[take..];
        }
    }

    fn emit_frame(&mut self) {
        let n = self.ring.len();
        // The frame ends at `samples`, so the oldest sample sits at `samples % n`.
        let start = (self.samples % n as u64) as usize;
        let (old, new) = self.ring.split_at(start);
        for ((b, &s), &w) in self.buf.iter_mut().zip(new.iter().chain(old)).zip(&self.window) {
            *b = Complex::new(s * w, 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        for (p, b) in self.power.iter_mut().zip(&self.buf) {
            *p = b.norm_sqr() + 1e-12;
        }
        let at = (self.frames % HISTORY as u64) as usize * BANDS;
        for band in 0..BANDS {
            let bins = &self.power[self.band_lo[band]..][..self.band_n[band]];
            let k = bins.len() as f64;
            let mean_ln = bins.iter().map(|p| p.ln()).sum::<f64>() / k;
            let mean = bins.iter().sum::<f64>() / k;
            let db = 10.0 * (mean_ln.exp() / mean).log10();
            self.history[at + band] = db.clamp(self.clip.0, self.clip.1) as f32;
        }
        self.frames += 1;
    }
}
