//! Causal fast/slow band power envelopes, one sample at a time.

/// Bands in `base.band_sos` order: low 45-140, body 140-400, upper 1-2k, 2-4k, 4-8k Hz.
pub(super) const BANDS: usize = 5;
pub(super) const LOW: usize = 0;
pub(super) const BODY: usize = 1;
pub(super) const UPPER: [usize; 3] = [2, 3, 4];
pub(super) const FAST: usize = 0;
pub(super) const SLOW: usize = 1;

/// Ports the band filters and one-pole smoothers of `kick_attack_rejection.causal_features` and
/// `kick_upper_cue.upper_features`: scipy `sosfilt` (zero initial state) of a 4th-order Butterworth
/// bandpass, squared, then `lfilter([1 - alpha], [1, -alpha])` per time constant. Causal band 2
/// (400-2000 Hz) is never read by the base features and is not run.
///
/// The `mul_add` placement reproduces the reference's arm64 build, where clang contracts
/// `sos0*x + z0`, `sos1*x - sos4*y` and `b0*x + z` into fused multiply-adds; with it the
/// envelopes match scipy bit for bit, so candidate edges (threshold tests) agree exactly.
pub(super) struct BandEnvelopes {
    sos: [[[f64; 6]; 2]; BANDS],
    z: [[[f64; 2]; 2]; BANDS],
    alpha: [f64; 2],
    gain: [f64; 2],
    env: [[f64; 2]; BANDS],
}

impl BandEnvelopes {
    pub(super) fn new(sos: &[f64], alpha: &[f64], gain: &[f64]) -> Self {
        let mut s = [[[0.0; 6]; 2]; BANDS];
        for (i, c) in sos.chunks_exact(6).enumerate() {
            s[i / 2][i % 2].copy_from_slice(c);
        }
        Self {
            sos: s,
            z: [[[0.0; 2]; 2]; BANDS],
            alpha: [alpha[0], alpha[1]],
            gain: [gain[0], gain[1]],
            env: [[0.0; 2]; BANDS],
        }
    }

    #[inline]
    pub(super) fn step(&mut self, x: f64) {
        for b in 0..BANDS {
            let mut cur = x;
            for s in 0..2 {
                let [b0, b1, b2, _, a1, a2] = self.sos[b][s];
                let z = &mut self.z[b][s];
                let y = b0.mul_add(cur, z[0]);
                z[0] = b1.mul_add(cur, -(a1 * y)) + z[1];
                z[1] = b2.mul_add(cur, -(a2 * y));
                cur = y;
            }
            let power = cur * cur;
            for t in [FAST, SLOW] {
                self.env[b][t] = self.gain[t].mul_add(power, self.alpha[t] * self.env[b][t]);
            }
        }
    }

    /// `[band][fast, slow]` after the last sample stepped.
    pub(super) fn values(&self) -> &[[f64; 2]; BANDS] {
        &self.env
    }
}
