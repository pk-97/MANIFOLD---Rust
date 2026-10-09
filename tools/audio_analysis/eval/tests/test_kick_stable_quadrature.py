import unittest

import numpy as np

from eval.kick_attack_rejection import causal_features
from eval.kick_stable_quadrature import stable_features


class KickStableQuadratureTests(unittest.TestCase):
    sample_rate = 48_000
    hop = 256

    def _envelopes(self, count):
        return np.full((count, 3, 2), 0.25, dtype=np.float64)

    def test_features_are_prefix_invariant(self):
        count = 240
        samples = np.random.default_rng(4).normal(0.0, 0.2, count * self.hop)
        envelopes = self._envelopes(count)
        prefix = stable_features(
            samples[:100 * self.hop], self.sample_rate, envelopes[:100], self.hop
        )
        complete = stable_features(samples, self.sample_rate, envelopes, self.hop)
        np.testing.assert_array_equal(prefix, complete[:100])

    def test_silence_is_zero_and_mid_is_copied(self):
        envelopes = self._envelopes(12)
        result = stable_features(
            np.zeros(len(envelopes) * self.hop), self.sample_rate, envelopes, self.hop
        )
        np.testing.assert_array_equal(result[:, 2], envelopes[:, 2])
        np.testing.assert_array_equal(result[:, :2], 0.0)
        np.testing.assert_array_equal(envelopes, self._envelopes(12))

    def test_band_center_sines_have_half_mean_power(self):
        count = 240
        sample_count = count * self.hop
        time = np.arange(sample_count) / self.sample_rate
        envelopes = self._envelopes(count)
        for band, center in enumerate((92.5, 270.0)):
            samples = np.sin(2.0 * np.pi * center * time)
            result = stable_features(samples, self.sample_rate, envelopes, self.hop)
            np.testing.assert_allclose(
                result[80:, band, 0], 0.5, rtol=0.03, atol=0.01
            )
            np.testing.assert_allclose(
                result[80:, band, 1], 0.5, rtol=0.03, atol=0.01
            )

    def test_steady_80hz_ripple_is_lower_than_old_fast_envelope(self):
        count = 240
        sample_count = count * self.hop
        time = np.arange(sample_count) / self.sample_rate
        samples = np.sin(2.0 * np.pi * 80.0 * time)
        envelopes = self._envelopes(count)
        stable = stable_features(samples, self.sample_rate, envelopes, self.hop)
        old = causal_features(samples, self.sample_rate)[0]
        stable_ripple = np.std(stable[80:, 0, 0])
        old_ripple = np.std(old[80:, 0, 0])
        self.assertLess(stable_ripple, old_ripple * 0.25)


if __name__ == "__main__":
    unittest.main()
