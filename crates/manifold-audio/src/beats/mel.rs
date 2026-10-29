//! Beat This!'s input spectrogram: a log-compressed mel spectrogram.
//! It reproduces torchaudio's `MelSpectrogram` settings (22050 Hz, 1024-point FFT, hop 441,
//! 30..11000 Hz, 128 Slaney mel bands, magnitude, frame-length normalised) followed by
//! `log1p(1000 * x)`, so the trained network sees exactly what it was trained on.

use rustfft::{FftPlanner, num_complex::Complex};

pub(super) const RATE: u32 = 22050;
pub(super) const HOP: usize = 441;
pub(super) const BANDS: usize = 128;
/// Frames per second of the spectrogram and of the network's outputs.
pub(super) const FPS: f64 = 50.0;

const N_FFT: usize = 1024;
const PAD: usize = N_FFT / 2;
const BINS: usize = N_FFT / 2 + 1;
const F_MIN: f64 = 30.0;
const F_MAX: f64 = 11000.0;

/// One mel band: the weights of bins `start..start + weights.len()`.
struct Band {
    start: usize,
    weights: Vec<f32>,
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
    let frames = 1 + len / HOP;

    let window: Vec<f32> = (0..N_FFT)
        .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos()) as f32)
        .collect();
    let bands = mel_filterbank();

    let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex::new(0.0f32, 0.0); N_FFT];
    let mut scratch = vec![Complex::new(0.0f32, 0.0); fft.get_inplace_scratch_len()];
    let mut mag = vec![0.0f32; BINS];
    let norm = 1.0 / (N_FFT as f32).sqrt();

    let mut out = Vec::with_capacity(frames * BANDS);
    for i in 0..frames {
        let frame = &padded[i * HOP..i * HOP + N_FFT];
        for (dst, (&x, &w)) in buf.iter_mut().zip(frame.iter().zip(&window)) {
            *dst = Complex::new(x * w, 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for (m, c) in mag.iter_mut().zip(&buf[..BINS]) {
            *m = c.norm() * norm;
        }
        for band in &bands {
            let mel: f32 = band
                .weights
                .iter()
                .zip(&mag[band.start..])
                .map(|(w, m)| w * m)
                .sum();
            out.push((1000.0 * mel).ln_1p());
        }
    }
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
