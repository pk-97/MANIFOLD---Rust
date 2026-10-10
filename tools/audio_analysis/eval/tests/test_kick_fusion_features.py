import unittest

import numpy as np

from eval.kick_fusion_features import FEATURE_NAMES, fusion_features


class KickFusionFeaturesTests(unittest.TestCase):
    def test_silence_has_empty_bounded_output(self):
        candidates, available, features, hop = fusion_features(np.zeros(1024), 48_000)
        self.assertEqual(hop, 256)
        self.assertEqual(candidates.shape, (0,))
        self.assertEqual(available.shape, (0,))
        self.assertEqual(features.shape, (0, len(FEATURE_NAMES)))

    def test_deterministic_pulses_are_finite_and_bounded(self):
        rng = np.random.default_rng(2)
        samples = rng.normal(0.0, 1e-4, 48_000)
        for start in (10_000, 22_000):
            samples[start : start + 512] += np.hanning(512) * 0.8
        candidates, available, features, hop = fusion_features(samples, 48_000)
        self.assertEqual(features.shape, (len(candidates), 9))
        self.assertEqual(np.all(np.isfinite(features)), True)
        self.assertTrue(np.all(available > candidates))
        self.assertTrue(np.all(available - candidates == 8))
        self.assertTrue(np.all(features[:, 0:3] >= -6.0))
        self.assertTrue(np.all(features[:, 0:3] <= 6.0))
        self.assertTrue(np.all((features[:, 3] >= 0.0) & (features[:, 3] <= 1.0)))
        self.assertTrue(np.all((features[:, 4:8] >= -6.0) & (features[:, 4:8] <= 6.0)))
        self.assertTrue(np.all((features[:, 8] >= 0.0) & (features[:, 8] <= 1.0)))

    def test_prefix_invariance_and_fixed_horizon(self):
        rng = np.random.default_rng(5)
        prefix = rng.normal(0.0, 0.1, 48_000)
        suffix = rng.normal(0.0, 0.1, 24_000)
        short = fusion_features(prefix, 48_000)
        full = fusion_features(np.concatenate((prefix, suffix)), 48_000)
        complete_prefix_hops = len(prefix) // short[3]
        keep = full[1] < complete_prefix_hops
        np.testing.assert_array_equal(short[0], full[0][keep])
        np.testing.assert_array_equal(short[1], full[1][keep])
        np.testing.assert_allclose(short[2], full[2][keep], atol=1e-12)
        self.assertTrue(np.all(short[1] - short[0] == 8))

    def test_gain_change_preserves_candidates_and_normalized_features(self):
        rng = np.random.default_rng(10)
        samples = rng.normal(0.0, 0.05, 48_000)
        samples[16_000 : 16_800] += np.hanning(800) * 0.5
        base = fusion_features(samples, 48_000)
        scaled = fusion_features(samples * 3.0, 48_000)
        np.testing.assert_array_equal(base[0], scaled[0])
        np.testing.assert_array_equal(base[1], scaled[1])
        np.testing.assert_allclose(base[2], scaled[2], atol=1e-4)


if __name__ == "__main__":
    unittest.main()
