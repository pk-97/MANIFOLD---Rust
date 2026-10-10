import unittest

import numpy as np

from eval.kick_attack_rejection import causal_features
from eval.kick_stable_rms import stable_features


class KickStableRmsTests(unittest.TestCase):
    sample_rate = 48_000
    hop = 256

    def _envelopes(self, count):
        return np.full((count, 3, 2), 7.0, dtype=np.float64)

    def test_features_are_prefix_invariant(self):
        samples = np.sin(2.0 * np.pi * 80.0 * np.arange(self.sample_rate) / self.sample_rate)
        envelopes = self._envelopes(len(samples) // self.hop)
        prefix = stable_features(samples[: self.sample_rate // 2], self.sample_rate,
                                 envelopes[: len(envelopes) // 2], self.hop)
        complete = stable_features(samples, self.sample_rate, envelopes, self.hop)
        np.testing.assert_array_equal(prefix, complete[: len(prefix)])

    def test_silence_is_zero_for_replaced_bands_and_preserves_mid(self):
        envelopes = self._envelopes(32)
        result = stable_features(np.zeros(self.sample_rate), self.sample_rate,
                                 envelopes, self.hop)
        np.testing.assert_array_equal(result[:, 2], envelopes[:, 2])
        self.assertTrue(np.all(result[:, :2] == 0.0))

    def test_stable_tone_has_less_ripple_than_existing_fast_envelope(self):
        samples = np.sin(2.0 * np.pi * 80.0 * np.arange(2 * self.sample_rate) / self.sample_rate)
        envelopes = self._envelopes(len(samples) // self.hop)
        stable = stable_features(samples, self.sample_rate, envelopes, self.hop)
        existing = causal_features(samples, self.sample_rate)[0]
        settled = slice(2 * self.sample_rate // self.hop // 2, None)
        stable_ripple = np.std(stable[settled, 0, 0])
        existing_ripple = np.std(existing[settled, 0, 0])
        self.assertLess(stable_ripple, existing_ripple)


if __name__ == "__main__":
    unittest.main()
