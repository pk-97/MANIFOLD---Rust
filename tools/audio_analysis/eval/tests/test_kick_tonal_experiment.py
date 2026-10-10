import unittest
from unittest.mock import patch

import numpy as np

from eval.kick_tonal_experiment import (
    SUPERFLUX_RATIO, _triangular_filterbank, _fft_size, flux_features,
    confirmation_diagnostics,
    confirmation_mask,
)


class KickTonalExperimentTests(unittest.TestCase):
    sample_rate = 48_000
    hop = 256

    def test_quantized_bank_has_no_empty_bands_or_coverage_holes(self):
        for sr in (44100, 48000):
            size = _fft_size(sr)
            bank = _triangular_filterbank(sr, size)
            freq = np.fft.rfftfreq(size, 1 / sr)
            self.assertTrue(np.all(bank.sum(axis=1) > 0))
            self.assertTrue(np.all(bank.sum(axis=0)[(freq > 30) & (freq < 2000)] > 0))

    def test_fft_batch_size_does_not_change_features(self):
        samples = np.random.default_rng(6).normal(0, .1, 320 * self.hop)
        ordinary, superflux = flux_features(samples, self.sample_rate, self.hop)
        with patch('eval.kick_tonal_experiment.FFT_BATCH_HOPS', 7):
            small_ordinary, small_superflux = flux_features(samples, self.sample_rate, self.hop)
        np.testing.assert_allclose(ordinary, small_ordinary, atol=1e-10)
        np.testing.assert_allclose(superflux, small_superflux, atol=1e-10)
        with self.assertRaises(ValueError):
            confirmation_mask(samples, self.sample_rate, self.hop, frame_count=321)

    def test_mask_is_prefix_invariant(self):
        rng = np.random.default_rng(17)
        prefix = rng.normal(0.0, 0.1, 48_000 // 4)
        suffix = rng.normal(0.0, 0.1, 48_000 // 5)
        prefix_mask = confirmation_mask(prefix, self.sample_rate, self.hop)
        full_mask = confirmation_mask(
            np.concatenate((prefix, suffix)), self.sample_rate, self.hop
        )
        np.testing.assert_array_equal(prefix_mask, full_mask[: len(prefix_mask)])

    def test_moving_tone_is_suppressed_but_new_transient_passes(self):
        duration_samples = self.sample_rate // 2
        time = np.arange(duration_samples, dtype=np.float64) / self.sample_rate
        # A constant-amplitude chirp moves through adjacent log bands.  The
        # neighbourhood maximum should account for most of that movement.
        moving_tone = 0.35 * np.sin(
            2.0 * np.pi * (350.0 * time + 0.5 * 1_500.0 * time * time)
        )
        moving = confirmation_diagnostics(moving_tone, self.sample_rate, self.hop)
        steady = moving["ratio"][20:]
        self.assertLess(float(np.median(steady)), SUPERFLUX_RATIO)

        # A short broadband attack creates energy outside the prior tonal
        # neighbourhood, so its ratio rises above the fixed pass threshold.
        burst_start = int(0.30 * self.sample_rate)
        burst_length = 1024
        burst = np.zeros(duration_samples)
        burst[burst_start : burst_start + burst_length] = (
            np.random.default_rng(23).normal(0.0, 1.0, burst_length)
            * np.hanning(burst_length)
            * 0.5
        )
        transient = confirmation_diagnostics(
            moving_tone + burst, self.sample_rate, self.hop
        )
        first_burst_hop = burst_start // self.hop
        response = transient["ratio"][first_burst_hop : first_burst_hop + 8]
        self.assertGreater(float(np.max(response)), 0.70)
        self.assertTrue(
            bool(np.any(transient["mask"][first_burst_hop : first_burst_hop + 8]))
        )


if __name__ == "__main__":
    unittest.main()
