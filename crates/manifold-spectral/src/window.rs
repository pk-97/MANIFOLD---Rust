//! Analysis windows. Ported verbatim from `manifold-analyzer-dsp` so the CQT
//! kernels match the Analyzer's spectrogram bit-for-bit.

/// Blackman-Harris window of `size` samples (4-term, −92 dB sidelobes). The CQT
/// kernel builder uses this so a narrow-band complex exponential FFTs to a
/// concentrated, low-leakage spectral kernel.
pub fn blackman_harris_window(size: usize) -> Vec<f32> {
    const A0: f32 = 0.35875;
    const A1: f32 = 0.48829;
    const A2: f32 = 0.14128;
    const A3: f32 = 0.01168;
    let denom = (size - 1).max(1) as f32;
    (0..size)
        .map(|n| {
            let x = 2.0 * std::f32::consts::PI * n as f32 / denom;
            A0 - A1 * x.cos() + A2 * (2.0 * x).cos() - A3 * (3.0 * x).cos()
        })
        .collect()
}
