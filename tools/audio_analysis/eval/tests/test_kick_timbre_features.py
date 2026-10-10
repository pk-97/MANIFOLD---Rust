import unittest

import numpy as np

from eval.kick_timbre_features import match_templates, timbre_features


SAMPLE_RATE = 48_000
HOP = 256


def _tone(frequency: float, duration: float = 0.75) -> np.ndarray:
    time = np.arange(int(SAMPLE_RATE * duration), dtype=np.float64) / SAMPLE_RATE
    return np.sin(2.0 * np.pi * frequency * time)


class TimbreFeatureTests(unittest.TestCase):
    def test_future_prefix_invariance(self):
        samples = _tone(80.0)
        prefix = timbre_features(samples, SAMPLE_RATE, HOP)
        extended = timbre_features(
            np.concatenate((samples, _tone(4_000.0, 0.4))), SAMPLE_RATE, HOP
        )
        np.testing.assert_array_equal(prefix, extended[: len(prefix)])

    def test_batch_size_does_not_change_features(self):
        samples = _tone(80.0)
        reference = timbre_features(samples, SAMPLE_RATE, HOP, batch_hops=7)
        np.testing.assert_array_equal(
            reference, timbre_features(samples, SAMPLE_RATE, HOP, batch_hops=256)
        )

    def test_gain_invariance_above_silence_floor(self):
        samples = _tone(80.0)
        np.testing.assert_allclose(
            timbre_features(samples, SAMPLE_RATE, HOP),
            timbre_features(0.25 * samples, SAMPLE_RATE, HOP),
            rtol=0.0,
            atol=1e-12,
        )

    def test_silence_is_zero(self):
        features = timbre_features(np.zeros(SAMPLE_RATE), SAMPLE_RATE, HOP)
        self.assertEqual(features.shape[1], 80)
        self.assertTrue(np.array_equal(features, np.zeros_like(features)))

    def test_low_and_high_tones_are_distinct(self):
        low = timbre_features(_tone(80.0), SAMPLE_RATE, HOP)
        high = timbre_features(_tone(4_000.0), SAMPLE_RATE, HOP)
        self.assertLess(float(np.sum(low[-1] * high[-1])), 0.25)


class TemplateMatcherTests(unittest.TestCase):
    def test_excludes_identical_same_track_distractor_and_returns_global_indices(self):
        query = np.array([1.0, 0.0])
        templates = np.array(
            [
                [1.0, 0.0],  # positive, excluded same-track distractor
                [0.8, 0.2],  # positive, selected
                [0.0, 1.0],  # negative
            ]
        )
        result = match_templates(
            query,
            templates,
            np.array([True, True, False]),
            np.array(["track-a", "track-b", "track-c"]),
            "track-a",
        )
        self.assertEqual(result["positive_index"], 1)
        self.assertEqual(result["negative_index"], 2)
        self.assertAlmostEqual(result["positive_similarity"], 0.8 / np.sqrt(0.68))
        self.assertAlmostEqual(
            result["margin"],
            result["positive_similarity"] - result["negative_similarity"],
        )

    def test_absent_class_and_zero_candidate_are_errors(self):
        query = np.array([1.0, 0.0])
        with self.assertRaises(ValueError):
            match_templates(
                query,
                np.array([[1.0, 0.0]]),
                np.array([True]),
                np.array(["track-a"]),
                "other",
            )
        with self.assertRaises(ValueError):
            match_templates(
                query,
                np.array([[0.0, 0.0], [0.0, 1.0]]),
                np.array([True, False]),
                np.array(["track-a", "track-b"]),
                "other",
            )


if __name__ == "__main__":
    unittest.main()
