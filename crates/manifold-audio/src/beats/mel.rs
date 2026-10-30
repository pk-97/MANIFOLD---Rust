//! Beat This!'s input spectrogram: a log-compressed mel spectrogram.
//! It reproduces torchaudio's `MelSpectrogram` settings (22050 Hz, 1024-point FFT, hop 441,
//! 30..11000 Hz, 128 Slaney mel bands, magnitude, frame-length normalised) followed by
//! `log1p(1000 * x)`, so the trained network sees exactly what it was trained on.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub(super) const RATE: u32 = 22050;
pub(super) const HOP: usize = 441;
pub(super) const BANDS: usize = 128;
/// Frames per second of the spectrogram and of the network's outputs.
pub(super) const FPS: f64 = 50.0;

/// Window length of one frame (samples at RATE).
pub(super) const N_FFT: usize = 1024;
const PAD: usize = N_FFT / 2;
const BINS: usize = N_FFT / 2 + 1;
const F_MIN: f64 = 30.0;
const F_MAX: f64 = 11000.0;

/// One mel band: the weights of bins `start..start + weights.len()`.
struct Band {
    start: usize,
    weights: Vec<f32>,
}

/// Streaming log-mel frames: frame i covers samples [i*HOP, i*HOP + N_FFT) of what was pushed, so it is centred
/// N_FFT/2 samples after its start.
pub(super) struct MelFramer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    bands: Vec<Band>,
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    mag: Vec<f32>,
    norm: f32,
    /// Samples from the start of the next frame onward.
    pending: Vec<f32>,
}

impl MelFramer {
    pub(super) fn new() -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let scratch = vec![Complex::new(0.0f32, 0.0); fft.get_inplace_scratch_len()];
        Self {
            fft,
            window: (0..N_FFT)
                .map(|n| {
                    (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos())
                        as f32
                })
                .collect(),
            bands: mel_filterbank(),
            buf: vec![Complex::new(0.0f32, 0.0); N_FFT],
            scratch,
            mag: vec![0.0f32; BINS],
            norm: 1.0 / (N_FFT as f32).sqrt(),
            pending: Vec::new(),
        }
    }

    /// Appends to `out` (row-major frames x BANDS) every frame whose window `samples` completes.
    pub(super) fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        self.pending.extend_from_slice(samples);
        let mut start = 0;
        while start + N_FFT <= self.pending.len() {
            let frame = &self.pending[start..start + N_FFT];
            for (dst, (&x, &w)) in self.buf.iter_mut().zip(frame.iter().zip(&self.window)) {
                *dst = Complex::new(x * w, 0.0);
            }
            self.fft
                .process_with_scratch(&mut self.buf, &mut self.scratch);
            for (m, c) in self.mag.iter_mut().zip(&self.buf[..BINS]) {
                *m = c.norm() * self.norm;
            }
            for band in &self.bands {
                let mel: f32 = band
                    .weights
                    .iter()
                    .zip(&self.mag[band.start..])
                    .map(|(w, m)| w * m)
                    .sum();
                out.push((1000.0 * mel).ln_1p());
            }
            start += HOP;
        }
        // Keep only what later frames still need.
        self.pending.drain(..start);
    }
}

/// Log-mel frames of `signal` (mono, RATE Hz), row-major frames x BANDS. Frame i is centred on sample i*HOP.
/// Empty when the signal is too short to reflect-pad (512 samples or fewer).
pub(super) fn log_mel(signal: &[f32]) -> Vec<f32> {
    let len = signal.len();
    if len <= PAD {
        return Vec::new();
    }
    let padded: Vec<f32> = (0..len + 2 * PAD)
        .map(|q| signal[reflect_index(q as isize - PAD as isize, len)])
        .collect();
    let mut out = Vec::with_capacity((1 + len / HOP) * BANDS);
    MelFramer::new().push(&padded, &mut out);
    out
}

/// Source sample for padded position `s` (relative to the signal start), reflecting without
/// repeating the edge sample, like torch's `pad_mode="reflect"`.
fn reflect_index(s: isize, len: usize) -> usize {
    if s < 0 {
        (-s) as usize
    } else if s as usize >= len {
        2 * (len - 1) - s as usize
    } else {
        s as usize
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    if hz >= 1000.0 {
        15.0 + (hz / 1000.0).ln() / (6.4f64.ln() / 27.0)
    } else {
        hz / f_sp
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    if mel >= 15.0 {
        1000.0 * ((6.4f64.ln() / 27.0) * (mel - 15.0)).exp()
    } else {
        f_sp * mel
    }
}

/// Triangular Slaney filterbank (torchaudio `melscale_fbanks`, no area norm), stored sparsely.
fn mel_filterbank() -> Vec<Band> {
    let all_freqs: Vec<f64> = (0..BINS)
        .map(|k| k as f64 * (RATE / 2) as f64 / (BINS - 1) as f64)
        .collect();
    let m_min = hz_to_mel(F_MIN);
    let m_max = hz_to_mel(F_MAX);
    let points = BANDS + 2;
    let f_pts: Vec<f64> = (0..points)
        .map(|i| mel_to_hz(m_min + (m_max - m_min) * i as f64 / (points - 1) as f64))
        .collect();
    let f_diff: Vec<f64> = f_pts.windows(2).map(|w| w[1] - w[0]).collect();

    (0..BANDS)
        .map(|m| {
            let weights_all: Vec<f32> = all_freqs
                .iter()
                .map(|&f| {
                    let down = (f - f_pts[m]) / f_diff[m];
                    let up = (f_pts[m + 2] - f) / f_diff[m + 1];
                    down.min(up).max(0.0) as f32
                })
                .collect();
            match weights_all.iter().position(|&w| w > 0.0) {
                Some(start) => {
                    let end = weights_all[start..]
                        .iter()
                        .position(|&w| w <= 0.0)
                        .map_or(BINS, |n| start + n);
                    Band {
                        start,
                        weights: weights_all[start..end].to_vec(),
                    }
                }
                None => Band {
                    start: 0,
                    weights: Vec::new(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_frames_match_whole() {
        let n = 2 * RATE as usize;
        let signal: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / RATE as f64;
                (0.4 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()
                    + 0.3 * (2.0 * std::f64::consts::PI * 1750.0 * t).sin()
                    + 0.2 * (2.0 * std::f64::consts::PI * 7300.0 * t).sin()) as f32
            })
            .collect();
        let whole = log_mel(&signal);
        assert_eq!(whole.len(), (1 + n / HOP) * BANDS);

        let padded: Vec<f32> = (0..n + 2 * PAD)
            .map(|q| signal[reflect_index(q as isize - PAD as isize, n)])
            .collect();
        for block in [300, 1, 5000] {
            let mut framer = MelFramer::new();
            let mut out = Vec::new();
            for chunk in padded.chunks(block) {
                framer.push(chunk, &mut out);
            }
            assert_eq!(out, whole, "block {block}");
        }
    }
}
