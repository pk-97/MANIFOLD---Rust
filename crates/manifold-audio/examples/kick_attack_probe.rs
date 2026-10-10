//! Standalone causal multiband kick-attack prototype.
//!
//! This is an offline experiment for comparing a causal kick detector against
//! labelled fixtures. It deliberately does not share code with the live
//! analysis path. The file mode prints the same sample-grid line and
//! `kick_hops` line consumed by `tools/audio_analysis/eval/live_kick_baseline.py`.
//!
//! ```text
//! cargo run -p manifold-audio --example kick_attack_probe -- <clip.wav>
//! cargo run -p manifold-audio --example kick_attack_probe -- --selftest
//! ```

use std::f64::consts::PI;

use manifold_playback::audio_decoder::decode_audio_to_pcm;
use rustfft::num_complex::Complex64;

const FAST_TAU_S: f64 = 0.003;
const SLOW_TAU_S: f64 = 0.080;
const MIN_FIRE_INTERVAL_S: f64 = 0.060;
const CONFIRM_TIMEOUT_S: f64 = 0.035;

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0,
            b1,
            b2,
            a1,
            a2,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f64) -> f64 {
        // Transposed direct-form II keeps the state bounded and needs no
        // scratch storage or allocation on the sample path.
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

#[derive(Clone, Copy)]
struct Bandpass {
    sections: [Biquad; 2],
}

impl Bandpass {
    fn new(lo_hz: f64, hi_hz: f64, sample_rate: f64) -> Self {
        // This is the analog Butterworth bandpass pole construction followed
        // by the bilinear transform used by scipy.signal.butter(...,
        // output="sos"). The two roots below each stand for a conjugate pole
        // pair and therefore define one real second-order section.
        let w1 = (PI * lo_hz / sample_rate).tan();
        let w2 = (PI * hi_hz / sample_rate).tan();
        let bandwidth = w2 - w1;
        let q = Complex64::new(-1.0, 1.0) * (bandwidth / 2.0_f64.sqrt());
        let discriminant = q * q - Complex64::new(4.0 * w1 * w2, 0.0);
        let root = discriminant.sqrt();
        let poles = [(q + root) / 2.0, (q - root) / 2.0];

        let mut sections = [
            Biquad::new(1.0, 0.0, -1.0, 0.0, 0.0),
            Biquad::new(1.0, 0.0, -1.0, 0.0, 0.0),
        ];
        for (section, pole) in sections.iter_mut().zip(poles) {
            let z = (Complex64::new(1.0, 0.0) + pole) / (Complex64::new(1.0, 0.0) - pole);
            section.a1 = -2.0 * z.re;
            section.a2 = z.norm_sqr();
        }

        // Normalize the cascade to unity at the geometric centre frequency.
        // The numerator zeros are at DC and Nyquist, as in the transformed
        // analog bandpass. Put the scalar on the first section so the two
        // sections keep the same combined transfer function as the reference.
        let center = 2.0 * (w1 * w2).sqrt().atan();
        let z_inv = Complex64::from_polar(1.0, -center);
        let mut response = Complex64::new(1.0, 0.0);
        for section in sections {
            let z_inv_sq = z_inv * z_inv;
            let numerator = Complex64::new(section.b0, 0.0)
                + Complex64::new(section.b1, 0.0) * z_inv
                + Complex64::new(section.b2, 0.0) * z_inv_sq;
            let denominator = Complex64::new(1.0, 0.0)
                + Complex64::new(section.a1, 0.0) * z_inv
                + Complex64::new(section.a2, 0.0) * z_inv_sq;
            response *= numerator / denominator;
        }
        let gain = 1.0 / response.norm();
        sections[0].b0 *= gain;
        sections[0].b2 *= gain;

        Self { sections }
    }

    #[inline]
    fn process(&mut self, input: f64) -> f64 {
        let mut value = input;
        for section in &mut self.sections {
            value = section.process(value);
        }
        value
    }
}

#[derive(Clone, Copy)]
struct Follower {
    fast_alpha: f64,
    slow_alpha: f64,
    fast: f64,
    slow: f64,
}

impl Follower {
    fn new(sample_rate: f64) -> Self {
        Self {
            fast_alpha: (-1.0 / (FAST_TAU_S * sample_rate)).exp(),
            slow_alpha: (-1.0 / (SLOW_TAU_S * sample_rate)).exp(),
            fast: 0.0,
            slow: 0.0,
        }
    }

    #[inline]
    fn push_power(&mut self, power: f64) {
        self.fast += (1.0 - self.fast_alpha) * (power - self.fast);
        self.slow += (1.0 - self.slow_alpha) * (power - self.slow);
    }

    #[inline]
    fn envelope(self) -> [f64; 2] {
        [self.fast, self.slow]
    }
}

struct Detector {
    sample_rate: f64,
    hop: usize,
    peak: f64,
    last_fire: f64,
    armed: bool,
    pending: Option<f64>,
    hop_count: usize,
}

impl Detector {
    fn new(sample_rate: f64, hop: usize) -> Self {
        Self {
            sample_rate,
            hop,
            peak: 0.0,
            last_fire: -1.0,
            armed: true,
            pending: None,
            hop_count: 0,
        }
    }

    /// Evaluate one completed hop. This is the fixed v5 state machine; it
    /// returns the event decision without allocating or looking ahead.
    #[inline]
    fn push_hop(&mut self, envelope: [[f64; 2]; 3]) -> bool {
        let dt = self.hop as f64 / self.sample_rate;
        let time = (self.hop_count + 1) as f64 * dt;
        self.hop_count += 1;

        let low = envelope[0];
        let body = envelope[1];
        let mid = envelope[2];
        let power = low[0] + body[0];
        let ratio = body[0] / (body[1] + 1e-12);
        self.peak = low[0].max(self.peak * (-dt / 2.0).exp());

        if ratio < 1.2 {
            self.armed = true;
        }
        let eligible = power > 1e-6
            && ratio > 2.0
            && power > 1.8 * (low[1] + body[1])
            && body[0] > low[0] / 3.0;

        if self.pending.is_none()
            && self.armed
            && eligible
            && time - self.last_fire >= MIN_FIRE_INTERVAL_S
        {
            self.pending = Some(time);
            self.armed = false;
        }

        // Confirmation is intentionally checked before timeout, matching the
        // Python reference when a hop reaches both conditions simultaneously.
        if self.pending.is_some() {
            if low[0] > 1e-6_f64.max(self.peak * 0.15)
                && low[0] > low[1] * 1.2
                && power > mid[0] * 0.8
            {
                self.last_fire = time;
                self.pending = None;
                return true;
            }
            if time - self.pending.unwrap_or(time) >= CONFIRM_TIMEOUT_S {
                self.pending = None;
            }
        }
        false
    }
}

/// Causal streaming processor. `push` emits zero-based completed-hop indices
/// through the callback; it retains all filter/follower state across chunks.
struct KickAttackProbe {
    sample_rate: u32,
    hop: usize,
    samples_in_hop: usize,
    bands: [Bandpass; 3],
    followers: [Follower; 3],
    detector: Detector,
    last_envelope: [[f64; 2]; 3],
}

impl KickAttackProbe {
    fn new(sample_rate: u32) -> Result<Self, String> {
        let sr = f64::from(sample_rate);
        if sample_rate <= 4_000 {
            return Err(format!(
                "sample rate {sample_rate} Hz is too low for a 2 kHz band"
            ));
        }
        let hop = (sr * 256.0 / 48_000.0).round().max(1.0) as usize;
        Ok(Self {
            sample_rate,
            hop,
            samples_in_hop: 0,
            bands: [
                Bandpass::new(45.0, 140.0, sr),
                Bandpass::new(140.0, 400.0, sr),
                Bandpass::new(400.0, 2_000.0, sr),
            ],
            followers: [Follower::new(sr), Follower::new(sr), Follower::new(sr)],
            detector: Detector::new(sr, hop),
            last_envelope: [[0.0; 2]; 3],
        })
    }

    #[inline]
    fn completed_hops(&self) -> usize {
        self.detector.hop_count
    }

    /// Push native-rate mono samples. No allocations occur inside the
    /// detector; the callback decides how to store any resulting indices.
    fn push<F: FnMut(usize)>(&mut self, samples: &[f32], mut on_fire: F) {
        for &sample in samples {
            let input = f64::from(sample);
            for (band, follower) in self.bands.iter_mut().zip(&mut self.followers) {
                let filtered = band.process(input);
                follower.push_power(filtered * filtered);
            }
            self.samples_in_hop += 1;
            if self.samples_in_hop == self.hop {
                self.samples_in_hop = 0;
                for (slot, follower) in self.followers.iter().enumerate() {
                    self.last_envelope[slot] = follower.envelope();
                }
                let hop_index = self.detector.hop_count;
                if self.detector.push_hop(self.last_envelope) {
                    on_fire(hop_index);
                }
            }
        }
    }
}

fn mono_samples(samples: &[f32], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

fn print_grid_and_fires(
    label: &str,
    sample_count: usize,
    probe: &KickAttackProbe,
    fires: &[usize],
) {
    let duration = sample_count as f64 / f64::from(probe.sample_rate);
    let dt_ms = 1_000.0 * probe.hop as f64 / f64::from(probe.sample_rate);
    println!(
        "{label}: {duration:.2}s @ {} Hz, {} hops of {} samples ({dt_ms:.2} ms)",
        probe.sample_rate,
        probe.completed_hops(),
        probe.hop,
    );
    println!("P3 {label}: kick_hops={fires:?}");
}

fn run_file(path: &str) -> Result<(), String> {
    let decoded = decode_audio_to_pcm(path)?;
    let mono = mono_samples(&decoded.samples, decoded.channels);
    let mut probe = KickAttackProbe::new(decoded.sample_rate)?;
    let mut fires = Vec::new();
    probe.push(&mono, |hop| fires.push(hop));
    print_grid_and_fires(path, mono.len(), &probe, &fires);
    Ok(())
}

fn synth_falling_pitch_kicks(sample_rate: u32) -> Vec<f32> {
    let sr = sample_rate as usize;
    let mut samples = vec![0.0; 3 * sr];
    for &start_s in &[0.25, 1.0, 1.75, 2.5] {
        let start = (start_s * f64::from(sample_rate)) as usize;
        let length = (0.22 * f64::from(sample_rate)) as usize;
        let mut phase = 0.0;
        for n in 0..length.min(samples.len().saturating_sub(start)) {
            let t = n as f64 / f64::from(sample_rate);
            let attack = 1.0 - (-t / 0.0015).exp();
            let decay = (-t / 0.095).exp();
            let frequency = 220.0 * (-t / 0.22 * (220.0_f64 / 55.0).ln()).exp();
            phase += 2.0 * PI * frequency / f64::from(sample_rate);
            let tone = phase.sin() * 0.82 + (2.0 * phase).sin() * 0.18;
            samples[start + n] += (attack * decay * tone * 0.8) as f32;
        }
    }
    samples
}

fn synth_stationary_bass_pulses(sample_rate: u32) -> Vec<f32> {
    let sr = sample_rate as usize;
    let mut samples = vec![0.0; 3 * sr];
    for (i, sample) in samples.iter_mut().enumerate() {
        let t = i as f64 / f64::from(sample_rate);
        let pulse_phase = (t / 0.5).fract();
        let pulse = if pulse_phase < 0.16 {
            (1.0 - (-pulse_phase * 0.5 / 0.004).exp()) * (-pulse_phase * 0.5 / 0.11).exp()
        } else {
            0.0
        };
        *sample = (0.65 * pulse * (2.0 * PI * 70.0 * t).sin()) as f32;
    }
    samples
}

fn synth_slow_bass_swell(sample_rate: u32) -> Vec<f32> {
    let sr = sample_rate as usize;
    let mut samples = vec![0.0; 3 * sr];
    for (i, sample) in samples.iter_mut().enumerate() {
        let t = i as f64 / f64::from(sample_rate);
        let amplitude = (t / 2.4).min(1.0) * 0.65;
        *sample = (amplitude * (2.0 * PI * 72.0 * t).sin()) as f32;
    }
    samples
}

fn run_selftest_case(name: &str, samples: &[f32], sample_rate: u32) -> Result<(), String> {
    let mut probe = KickAttackProbe::new(sample_rate)?;
    let mut fires = Vec::new();
    probe.push(samples, |hop| fires.push(hop));
    println!("selftest {name}: event_count={}", fires.len());
    Ok(())
}

fn run_selftest() -> Result<(), String> {
    const SR: u32 = 48_000;
    run_selftest_case("silence", &vec![0.0; 3 * SR as usize], SR)?;
    let kicks = synth_falling_pitch_kicks(SR);
    run_selftest_case("falling_pitch_kicks", &kicks, SR)?;
    let pulses = synth_stationary_bass_pulses(SR);
    run_selftest_case("stationary_bass_pulses", &pulses, SR)?;
    let swell = synth_slow_bass_swell(SR);
    run_selftest_case("slow_bass_swell", &swell, SR)
}

fn main() {
    let mut file = None;
    let mut selftest = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--selftest" => selftest = true,
            s if s.starts_with("--") => {
                eprintln!("unknown flag {s}");
                std::process::exit(2);
            }
            s if file.is_none() => file = Some(s.to_string()),
            _ => {
                eprintln!("usage: kick_attack_probe <file> | --selftest");
                std::process::exit(2);
            }
        }
    }

    let result = if selftest {
        run_selftest()
    } else if let Some(path) = file.as_deref() {
        run_file(path)
    } else {
        Err("usage: kick_attack_probe <file> | --selftest".into())
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(samples: &[f32], sample_rate: u32, chunks: &[usize]) -> Vec<usize> {
        let mut probe = KickAttackProbe::new(sample_rate).expect("valid rate");
        let mut fires = Vec::new();
        let mut offset = 0;
        for &size in chunks {
            let end = (offset + size).min(samples.len());
            probe.push(&samples[offset..end], |hop| fires.push(hop));
            offset = end;
            if offset == samples.len() {
                break;
            }
        }
        if offset < samples.len() {
            probe.push(&samples[offset..], |hop| fires.push(hop));
        }
        fires
    }

    #[test]
    fn silence_is_finite_and_zero() {
        let mut probe = KickAttackProbe::new(48_000).expect("valid rate");
        let mut fires = Vec::new();
        probe.push(&vec![0.0; 48_000], |hop| fires.push(hop));
        assert!(fires.is_empty());
        assert!(
            probe
                .last_envelope
                .into_iter()
                .flatten()
                .all(|value| value.is_finite() && value == 0.0)
        );
    }

    #[test]
    fn split_input_is_identical_for_irregular_chunks() {
        let samples = synth_falling_pitch_kicks(48_000);
        let expected = run(&samples, 48_000, &[samples.len()]);
        let actual = run(&samples, 48_000, &[1, 7, 31, 256, 19, 503, 2, 4096]);
        assert_eq!(actual, expected);
    }

    #[test]
    fn no_fire_before_a_complete_hop() {
        let mut probe = KickAttackProbe::new(48_000).expect("valid rate");
        let mut fires = Vec::new();
        let prefix = vec![1.0; probe.hop - 1];
        probe.push(&prefix, |hop| fires.push(hop));
        assert!(fires.is_empty());
        assert_eq!(probe.completed_hops(), 0);
        probe.push(&[0.0], |hop| fires.push(hop));
        assert_eq!(probe.completed_hops(), 1);
    }

    #[test]
    fn hop_scales_with_sample_rate() {
        assert_eq!(KickAttackProbe::new(44_100).unwrap().hop, 235);
        assert_eq!(KickAttackProbe::new(48_000).unwrap().hop, 256);
        assert_eq!(KickAttackProbe::new(96_000).unwrap().hop, 512);
    }

    #[test]
    fn repeated_kicks_produce_one_event_each_at_native_rates() {
        for sample_rate in [44_100, 48_000, 96_000] {
            let samples = synth_falling_pitch_kicks(sample_rate);
            let fires = run(&samples, sample_rate, &[samples.len()]);
            let hop = KickAttackProbe::new(sample_rate).unwrap().hop;
            assert_eq!(fires.len(), 4, "rate {sample_rate}");
            for (index, onset) in fires.iter().zip([0.25, 1.0, 1.75, 2.5]) {
                let available = (index + 1) as f64 * hop as f64 / f64::from(sample_rate);
                assert!((onset..onset + 0.100).contains(&available));
            }
        }
    }
}
