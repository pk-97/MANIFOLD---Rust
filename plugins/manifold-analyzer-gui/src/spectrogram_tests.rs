//! End-to-end worker fixtures; GPU proofs reuse these exact output columns.
use super::*;

pub(crate) fn noise(rate: f32, pink: bool) -> Vec<f32> {
    use rustfft::{num_complex::Complex, FftPlanner};
    let n = 131072;
    let mut spectrum = vec![Complex::new(0.0, 0.0); n];
    let mut rng = 0x73c849d1_u32;
    for k in 1..n / 2 {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        let phase = std::f64::consts::TAU * rng as f64 / u32::MAX as f64;
        let frequency = k as f64 * rate as f64 / n as f64;
        // Independently specified one-sided PSD. White has RMS=0.1 at 48 kHz;
        // pink equals white at 1 kHz, with power proportional to 1/f.
        let psd = 0.02 / 48000.0 * if pink { 1000.0 / frequency } else { 1.0 };
        let magnitude = (psd * rate as f64 * n as f64 / 2.0).sqrt() as f32;
        spectrum[k] = Complex::from_polar(magnitude, phase as f32);
        spectrum[n - k] = spectrum[k].conj();
    }
    FftPlanner::new().plan_fft_inverse(n).process(&mut spectrum);
    spectrum.into_iter().map(|v| v.re / n as f32).collect()
}

pub(crate) fn columns(audio: &[f32], rate: f32, sharpen: bool) -> (Vec<f32>, Vec<Vec<f32>>) {
    let params = crate::spectrum_gpu::cqt_build_params(rate);
    let cqt = CqtTransform::new(
        rate,
        params.n_fft,
        params.fmin,
        params.fmax,
        params.bpo,
        params.gamma_lo,
        params.gamma_hi,
        params.gamma_transition,
        params.causal,
        params.threshold_rel,
    );
    let frequencies = cqt.center_freqs().to_vec();
    let bins = cqt.num_bins();
    let pool = Arc::new(ArrayQueue::new(8));
    for _ in 0..8 {
        pool.push(CqtColumnMsg {
            history_epoch: 0,
            col_idx: 0,
            data: vec![WORKER_FLOOR_DB; bins],
            secondary_active: false,
            data2: None,
            clear_history_before: false,
        })
        .ok();
    }
    let mut state = WorkerState::new(cqt, params, rate, pool.clone());
    let output = ArrayQueue::new(8);
    let cfg = WorkerConfig {
        synchrosqueeze: sharpen,
        ..WorkerConfig::OFF
    };
    let mut result = Vec::new();
    for (block, chunk) in audio.chunks(256).enumerate() {
        let samples: Vec<_> = chunk
            .iter()
            .enumerate()
            .map(|(i, &x)| StereoSample {
                left: x,
                right: x,
                index: (block * 256 + i) as u64,
                generation: 1,
                sample_rate: rate,
                beat: f64::NAN,
                bpm: 0.0,
            })
            .collect();
        state.process(&samples, &cfg, &output);
        while let Some(col) = output.pop() {
            result.push(col.data.clone());
            pool.push(col).ok();
        }
    }
    (frequencies, result)
}

#[test]
fn density_display_has_no_high_frequency_noise_gain() {
    let expected = 10.0_f32 * (2.0_f32 * (0.02 / 48000.0) * 100.0).log10();
    for rate in [44100.0, 48000.0, 96000.0] {
        for pink in [false, true] {
            let audio = noise(rate, pink);
            let (frequencies, columns) = columns(&audio[..rate as usize], rate, false);
            let settled = &columns[(rate as usize / 4) / 256..];
            for frequency in [250.0, 1000.0, 5000.0, 10000.0, 18000.0] {
                let bin = frequencies
                    .iter()
                    .enumerate()
                    .min_by(|a, b| (a.1 - frequency).abs().total_cmp(&(b.1 - frequency).abs()))
                    .unwrap()
                    .0;
                let power = settled
                    .iter()
                    .map(|c| 10.0_f64.powf(c[bin] as f64 * 0.1))
                    .sum::<f64>()
                    / settled.len() as f64;
                let tilt = if pink {
                    3.0 * (frequencies[bin] / 1000.0).log2()
                } else {
                    0.0
                };
                let db = 10.0 * power.log10() as f32 + tilt;
                eprintln!(
                    "{rate}Hz pink={pink} at {}Hz: {db:.2}dB expected {expected:.2}",
                    frequencies[bin]
                );
                assert!(
                    (db - expected).abs() < 1.3,
                    "noise colour bias at {frequency}Hz: {db}"
                );
            }
        }
    }
}

#[test]
fn column_reduction_is_mean_power_and_reset_drops_old_energy() {
    let mut channel = ChannelState::new(4, 1);
    channel.cqt_out[0] = 0.0;
    channel.accumulate_column();
    for _ in 1..16 {
        channel.cqt_out[0] = WORKER_FLOOR_DB;
        channel.accumulate_column();
    }
    channel.finish_column();
    assert!((channel.column_average[0] - 10.0_f32 * (1.0_f32 / 16.0).log10()).abs() < 0.001);
    channel.reset_history();
    channel.finish_column();
    assert_eq!(channel.column_average[0], WORKER_FLOOR_DB);
}

#[test]
fn sharpen_redistribution_preserves_density_integral() {
    let rate = 48000.0;
    let params = crate::spectrum_gpu::cqt_build_params(rate);
    let cqt = CqtTransform::new(
        rate,
        params.n_fft,
        params.fmin,
        params.fmax,
        params.bpo,
        params.gamma_lo,
        params.gamma_hi,
        params.gamma_transition,
        params.causal,
        params.threshold_rel,
    );
    let bins = cqt.num_bins();
    let hop = params.hop_samples;
    let mut now = vec![CqtComplex::new(0.0, 0.0); bins];
    let mut prev = now.clone();
    // Each bin carries an on-centre stationary phasor; reassignment must not
    // manufacture or discard energy when every bin passes the phase gate.
    for (k, &f) in cqt.center_freqs().iter().enumerate() {
        prev[k] = CqtComplex::new(0.1, 0.0);
        now[k] = CqtComplex::from_polar(0.1, std::f32::consts::TAU * f * hop as f32 / rate);
    }
    let cfg = WorkerConfig {
        coherence: false,
        ..WorkerConfig::OFF
    };
    let mut scratch = vec![0.0; bins];
    let mut out = vec![0.0; bins];
    synchrosqueeze_into(
        cqt.center_freqs(),
        cqt.bandwidths_hz(),
        cqt.display_power_gains(),
        &now,
        &prev,
        &prev,
        false,
        &cfg,
        hop,
        rate,
        bins,
        &mut scratch,
        &mut out,
    );
    let before: f64 = cqt
        .center_freqs()
        .iter()
        .zip(cqt.display_power_gains())
        .map(|(&f, &g)| f as f64 * g as f64 * 0.01)
        .sum();
    let after: f64 = cqt
        .center_freqs()
        .iter()
        .zip(out)
        .map(|(&f, db)| f as f64 * 10.0_f64.powf(db as f64 * 0.1))
        .sum();
    assert!(
        (after / before - 1.0).abs() < 1e-4,
        "density integral changed: {before} -> {after}"
    );
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn column_message(
    col_idx: u32,
    data: Vec<f32>,
    data2: Option<Vec<f32>>,
) -> CqtColumnMsg {
    CqtColumnMsg {
        history_epoch: 1,
        col_idx,
        data,
        secondary_active: data2.is_some(),
        data2,
        clear_history_before: false,
    }
}
