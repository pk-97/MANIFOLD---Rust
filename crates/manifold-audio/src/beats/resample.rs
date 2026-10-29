//! Kaiser-windowed sinc resampling of a whole mono song. Each output sample is a
//! low-pass evaluated at its exact source position; the beat network was trained
//! on soxr-resampled 22050 Hz audio, so aliasing into 0-11 kHz must stay negligible.

use std::f64::consts::PI;

/// Zero crossings of the sinc kernel on each side of the centre.
const ZEROS: f64 = 16.0;
/// Pass band edge as a share of the lower of the two Nyquist rates.
const ROLLOFF: f64 = 0.94;
/// Kaiser shape parameter, about 90 dB of stopband attenuation.
const BETA: f64 = 8.6;

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Modified Bessel function of order 0 by its power series.
fn bessel_i0(x: f64) -> f64 {
    let q = x * 0.5;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        let f = q / k;
        term *= f * f;
        sum += term;
        if term < 1e-12 * sum {
            return sum;
        }
        k += 1.0;
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// `x` (mono, `from` Hz) at `to` Hz.
pub(super) fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return x.to_vec();
    }
    let g = gcd(from, to);
    let up = (to / g) as usize;
    let down = (from / g) as usize;
    let out_len = (x.len() * up).div_ceil(down);

    let fc = 0.5 * ROLLOFF * (to as f64 / from as f64).min(1.0);
    let half_width = ZEROS / (2.0 * fc);
    let reach = half_width.ceil() as isize;
    let taps = (2 * reach + 2) as usize;
    let i0_beta = bessel_i0(BETA);

    // Phase-major kernel table: row p holds the weights for source offsets
    // -reach..=reach+1 around a position p/up past a source sample.
    let mut table = Vec::with_capacity(up * taps);
    for p in 0..up {
        let frac = p as f64 / up as f64;
        for j in -reach..=reach + 1 {
            let d = j as f64 - frac;
            let r = d / half_width;
            let w = if r.abs() <= 1.0 {
                bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0_beta
            } else {
                0.0
            };
            table.push((2.0 * fc * sinc(2.0 * fc * d) * w) as f32);
        }
    }

    let len = x.len() as isize;
    let mut y = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let pos = n * down;
        let base = (pos / up) as isize;
        let p = pos % up;
        let jlo = (-reach).max(-base);
        let jhi = (reach + 1).min(len - 1 - base);
        if jlo > jhi {
            y.push(0.0);
            continue;
        }
        let xs = &x[(base + jlo) as usize..=(base + jhi) as usize];
        let row = &table[p * taps..(p + 1) * taps];
        let ts = &row[(jlo + reach) as usize..=(jhi + reach) as usize];
        y.push(xs.iter().zip(ts).map(|(a, b)| a * b).sum());
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (0.5 * (2.0 * PI * freq * i as f64 / rate as f64).sin()) as f32)
            .collect()
    }

    fn middle_rms(y: &[f32]) -> f64 {
        let m = &y[y.len() / 4..y.len() * 3 / 4];
        (m.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / m.len() as f64).sqrt()
    }

    #[test]
    fn keeps_a_passband_tone_and_drops_an_alias() {
        let want = 0.5 / 2f64.sqrt();
        let y = resample(&sine(1000.0, 48000, 24000), 48000, 22050);
        assert_eq!(y.len(), (24000usize * 147).div_ceil(320));
        assert!((middle_rms(&y) / want - 1.0).abs() < 0.01);

        let y = resample(&sine(15000.0, 48000, 24000), 48000, 22050);
        assert!(middle_rms(&y) < want * 1e-3);
    }

    #[test]
    fn halves_44100_exactly() {
        let y = resample(&sine(2000.0, 44100, 22051), 44100, 22050);
        assert_eq!(y.len(), 22051usize.div_ceil(2));
        assert!((middle_rms(&y) / (0.5 / 2f64.sqrt()) - 1.0).abs() < 0.01);
    }

    #[test]
    fn same_rate_is_identity() {
        let x = sine(440.0, 22050, 100);
        assert_eq!(resample(&x, 22050, 22050), x);
    }
}
