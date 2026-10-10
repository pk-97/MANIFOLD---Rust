import unittest

import numpy as np

from eval.kick_pcen_experiment import detect, pcen_features


class KickPcenExperimentTests(unittest.TestCase):
    sample_rate = 48_000
    hop = 256

    def _envelopes(self, count=32):
        env = np.zeros((count, 3, 2), dtype=np.float64)
        env[:, :, 0] = 0.01
        env[:, :, 1] = 0.01
        env[8, 0, 0] = 1.0
        env[8, 1, 0] = 0.6
        env[8, 2, 0] = 0.05
        return env

    def test_features_are_prefix_invariant(self):
        env = self._envelopes()
        prefix = pcen_features(env[:16], self.sample_rate, self.hop)
        complete = pcen_features(env, self.sample_rate, self.hop)
        for key in ("pcen", "background", "novelty"):
            np.testing.assert_array_equal(prefix[key], complete[key][:16])
        np.testing.assert_array_equal(
            prefix["eligible_mask"], complete["eligible_mask"][:16]
        )
        np.testing.assert_array_equal(
            prefix["rearm_mask"], complete["rearm_mask"][:16]
        )
        self.assertEqual(
            detect(None, self.sample_rate, env[:16], self.hop),
            [index for index in detect(None, self.sample_rate, env, self.hop)
             if index < 16],
        )

    def test_silence_has_no_novelty_or_fires(self):
        env = np.zeros((24, 3, 2), dtype=np.float64)
        features = pcen_features(env, self.sample_rate, self.hop)
        self.assertTrue(np.all(features["pcen"] == 0.0))
        self.assertTrue(np.all(features["novelty"] == 0.0))
        self.assertFalse(np.any(features["eligible_mask"]))
        self.assertTrue(np.all(features["rearm_mask"]))
        self.assertEqual(detect(None, self.sample_rate, env, self.hop), [])

    def test_level_scaling_preserves_candidate_and_fire_hops(self):
        env = self._envelopes()
        scaled = env * 4.0
        base = pcen_features(env, self.sample_rate, self.hop)
        scaled_features = pcen_features(scaled, self.sample_rate, self.hop)
        np.testing.assert_array_equal(
            base["eligible_mask"], scaled_features["eligible_mask"]
        )
        np.testing.assert_array_equal(
            base["rearm_mask"], scaled_features["rearm_mask"]
        )
        # The finite PCEN epsilon intentionally permits small novelty changes
        # under scaling, while the fixed candidate/rearm decisions stay stable
        # for ordinary signal levels.
        self.assertEqual(
            detect(None, self.sample_rate, env, self.hop),
            detect(None, self.sample_rate, scaled, self.hop),
        )


if __name__ == "__main__":
    unittest.main()
