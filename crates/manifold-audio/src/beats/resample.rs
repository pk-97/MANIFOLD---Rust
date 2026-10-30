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

/// Streaming resampler: push input blocks, get every output sample whose taps have all arrived.
pub(super) struct Resampler {
    passthrough: bool,
    up: usize,
    down: usize,
    reach: isize,
    taps: usize,
    /// Phase-major kernel table: row p holds the weights for source offsets
    /// -reach..=reach+1 around a position p/up past a source sample.
    table: Vec<f32>,
    /// Recent input; `history[0]` is absolute input index `history_start`.
    history: Vec<f32>,
    history_start: usize,
    /// Input samples received so far.
    received: usize,
    /// Index of the next output sample to produce.
    next_out: usize,
}

impl Resampler {
    pub(super) fn new(from: u32, to: u32) -> Self {
        if from == to {
            return Self {
                passthrough: true,
                up: 1,
                down: 1,
                reach: 0,
                taps: 0,
                table: Vec::new(),
                history: Vec::new(),
                history_start: 0,
                received: 0,
                next_out: 0,
            };
        }
        let g = gcd(from, to);
        let up = (to / g) as usize;
        let down = (from / g) as usize;

        let fc = 0.5 * ROLLOFF * (to as f64 / from as f64).min(1.0);
        let half_width = ZEROS / (2.0 * fc);
        let reach = half_width.ceil() as isize;
        let taps = (2 * reach + 2) as usize;
        let i0_beta = bessel_i0(BETA);

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
        Self {
            passthrough: false,
            up,
            down,
            reach,
            taps,
            table,
            history: Vec::new(),
            history_start: 0,
            received: 0,
            next_out: 0,
        }
    }

    /// Appends to `out` the output samples `input` completes.
    pub(super) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.history.extend_from_slice(input);
        self.received += input.len();
        loop {
            let base = self.next_out * self.down / self.up;
            // The last tap, input index base+reach+1, must have arrived.
            if base + self.reach as usize + 1 >= self.received {
                break;
            }
            out.push(self.output(self.next_out));
            self.next_out += 1;
        }
        // Drop input before the first tap of the next output.
        let base = self.next_out * self.down / self.up;
        let keep_from = base.saturating_sub(self.reach as usize);
        let drop = keep_from
            .saturating_sub(self.history_start)
            .min(self.history.len());
        self.history.drain(..drop);
        self.history_start += drop;
    }

    /// Appends the outputs still owed, treating the input past its end as silence.
    pub(super) fn finish(&mut self, out: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        let out_len = (self.received * self.up).div_ceil(self.down);
        while self.next_out < out_len {
            out.push(self.output(self.next_out));
            self.next_out += 1;
        }
    }

    /// Output sample `n` from the in-range taps; input outside `0..received` is 0.
    fn output(&self, n: usize) -> f32 {
        let pos = n * self.down;
        let base = (pos / self.up) as isize;
        let p = pos % self.up;
        let jlo = (-self.reach).max(-base);
        let jhi = (self.reach + 1).min(self.received as isize - 1 - base);
        if jlo > jhi {
            return 0.0;
        }
        let first = (base + jlo) as usize - self.history_start;
        let last = (base + jhi) as usize - self.history_start;
        let xs = &self.history[first..=last];
        let row = &self.table[p * self.taps..(p + 1) * self.taps];
        let ts = &row[(jlo + self.reach) as usize..=(jhi + self.reach) as usize];
        xs.iter().zip(ts).map(|(a, b)| a * b).sum()
    }
}

/// `x` (mono, `from` Hz) at `to` Hz.
pub(super) fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    let mut r = Resampler::new(from, to);
    let mut out = Vec::new();
    r.push(x, &mut out);
    r.finish(&mut out);
    out
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

    #[test]
    fn streaming_in_blocks_matches_whole() {
        let x: Vec<f32> = sine(440.0, 48000, 14400)
            .iter()
            .zip(sine(3100.0, 48000, 14400))
            .zip(sine(9000.0, 48000, 14400))
            .map(|((a, b), c)| a + 0.5 * b + 0.25 * c)
            .collect();
        let whole = resample(&x, 48000, 22050);
        for block in [97, 1000, 1, 4096] {
            let mut r = Resampler::new(48000, 22050);
            let mut out = Vec::new();
            for chunk in x.chunks(block) {
                r.push(chunk, &mut out);
            }
            r.finish(&mut out);
            assert_eq!(out, whole, "block {block}");
        }
    }
}
