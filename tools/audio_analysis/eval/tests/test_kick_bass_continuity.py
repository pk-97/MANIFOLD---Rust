import unittest

import numpy as np

from eval.kick_bass_continuity import bandpass_subsample, score_event, score_window


class KickBassContinuityTests(unittest.TestCase):
    def test_stationary_sine_is_predictable(self):
        sr = 2_000
        signal = np.sin(2 * np.pi * 100 * np.arange(800) / sr)
        result = score_window(signal, 800, sr)
        self.assertEqual(result["status"], "veto")
        self.assertLessEqual(result["score"], 0.10)

    def test_abrupt_innovation_is_kept(self):
        sr = 2_000
        time = np.arange(800) / sr
        signal = np.sin(2 * np.pi * 100 * time)
        signal[-40:] += 2.0 * np.sin(2 * np.pi * 337 * time[-40:])
        result = score_window(signal, 800, sr)
        self.assertEqual(result["status"], "keep")
        self.assertGreater(result["score"], 0.10)

    def test_past_only_invariance(self):
        sr = 2_000
        prefix = np.sin(2 * np.pi * 100 * np.arange(800) / sr)
        suffix = np.random.default_rng(4).normal(size=500)
        before = score_event(prefix, sr, 0.400)
        after = score_event(np.concatenate((prefix, suffix)), sr, 0.400)
        self.assertEqual(before["status"], after["status"])
        self.assertAlmostEqual(before["score"], after["score"], places=14)

    def test_filter_records_actual_subsample_rate(self):
        sr = 44_100
        signal = np.zeros(sr)
        filtered, actual_sr, step = bandpass_subsample(signal, sr)
        self.assertEqual(step, round(sr / 2_000))
        self.assertEqual(actual_sr, sr / step)
        self.assertEqual(len(filtered), (len(signal) - 1) // step + 1)


if __name__ == "__main__":
    unittest.main()
