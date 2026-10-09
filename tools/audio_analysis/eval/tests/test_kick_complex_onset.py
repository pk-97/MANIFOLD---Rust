"""Numerical tests for the causal complex-domain onset measurement."""

from __future__ import annotations

import unittest

import numpy as np

from eval.kick_complex_onset import complex_features, _prediction_error


class KickComplexOnsetTests(unittest.TestCase):
    sample_rate = 48_000
    hop = 256

    def test_empty_silence_and_first_two_outputs(self):
        empty = complex_features(np.zeros(0), self.sample_rate, self.hop)
        self.assertEqual(len(empty["raw"]), 0)
        self.assertEqual(len(empty["normalized"]), 0)

        silence = complex_features(
            np.zeros(7 * self.hop), self.sample_rate, self.hop
        )
        np.testing.assert_array_equal(silence["raw"], 0.0)
        np.testing.assert_array_equal(silence["normalized"], 0.0)
        np.testing.assert_array_equal(silence["raw"][:2], 0.0)
        np.testing.assert_array_equal(silence["normalized"][:2], 0.0)

    def test_rejects_nonfinite_nonreal_or_invalid_arguments(self):
        with self.assertRaises(ValueError):
            complex_features(np.array([0.0, np.nan]), self.sample_rate, self.hop)
        with self.assertRaises(ValueError):
            complex_features(np.array([1.0 + 0.5j]), self.sample_rate, self.hop)
        with self.assertRaises(ValueError):
            complex_features(np.zeros((2, 2)), self.sample_rate, self.hop)
        for args in ((0, self.hop, 256), (self.sample_rate, 0, 256),
                     (self.sample_rate, self.hop, 0)):
            with self.assertRaises(ValueError):
                complex_features(np.zeros(self.hop), *args)

    def test_prefix_and_batch_invariance(self):
        rng = np.random.default_rng(19)
        prefix = rng.normal(0.0, 0.1, 73 * self.hop + 17)
        suffix = rng.normal(0.0, 0.1, 41 * self.hop + 31)
        full = np.concatenate((prefix, suffix))

        prefix_result = complex_features(prefix, self.sample_rate, self.hop)
        full_result = complex_features(full, self.sample_rate, self.hop)
        for key in ("raw", "normalized"):
            np.testing.assert_allclose(
                prefix_result[key], full_result[key][: len(prefix_result[key])],
                atol=1e-12,
                rtol=0.0,
            )

        explicit_small_batch = complex_features(
            full, self.sample_rate, self.hop, batch_hops=7
        )
        for key in ("raw", "normalized"):
            np.testing.assert_allclose(
                full_result[key], explicit_small_batch[key], atol=1e-12, rtol=0.0
            )

    def test_gain_invariance_above_normalization_floor(self):
        rng = np.random.default_rng(23)
        samples = rng.normal(0.0, 0.2, 50 * self.hop)
        base = complex_features(samples, self.sample_rate, self.hop)
        gained = complex_features(3.75 * samples, self.sample_rate, self.hop)
        np.testing.assert_allclose(
            base["normalized"], gained["normalized"], atol=1e-12, rtol=1e-12
        )
        self.assertTrue(np.all((base["normalized"] >= 0.0) & (base["normalized"] <= 1.0)))
        self.assertTrue(np.any(base["raw"][2:] > 1e-8))
        np.testing.assert_allclose(
            gained["raw"], 3.75 * base["raw"], atol=1e-12, rtol=1e-12
        )

    def test_complex_prediction_matches_analytic_phase_reset_helper(self):
        # Constant phase increments predict phase .7 from .1 and .4.
        # A pi reset moves a magnitude-2 bin to the opposite point (distance 4).
        # A same-phase magnitude increase from 3 to 4 has distance 1.
        older = np.array([2.,3.])*np.exp(.1j)
        previous = np.array([2.,3.])*np.exp(.4j)
        stable = np.array([2.,3.])*np.exp(.7j)
        np.testing.assert_allclose(_prediction_error(stable,previous,older),0,atol=1e-12)
        changed = np.array([-2.,4.])*np.exp(.7j)
        np.testing.assert_allclose(_prediction_error(changed,previous,older),[4,1],atol=1e-12)

    def test_stationary_sine_is_quieter_than_transient_waveform(self):
        duration_hops = 100
        n_samples = duration_hops * self.hop
        time = np.arange(n_samples, dtype=np.float64) / self.sample_rate
        sine = 0.3 * np.sin(2.0 * np.pi * 220.0 * time)
        transient = sine.copy()
        transient[40 * self.hop : 40 * self.hop + self.hop] += np.hanning(self.hop)

        steady = complex_features(sine, self.sample_rate, self.hop)["normalized"]
        attacked = complex_features(transient, self.sample_rate, self.hop)["normalized"]
        self.assertLess(float(np.median(steady[20:35])), 0.05)
        self.assertGreater(
            float(np.max(attacked[39:44])), float(np.max(steady[39:44])) + 0.1
        )


if __name__ == "__main__":
    unittest.main()
