import unittest

import numpy as np

from eval import kick_fusion_features as base
from eval.kick_fusion_bandwise import (
    BANDS,
    FEATURE_NAMES,
    _bandwise_features,
    fusion_features,
)


class KickFusionBandwiseTests(unittest.TestCase):
    def test_frozen_columns_and_grid_are_bitwise_identical(self):
        self.assertEqual(FEATURE_NAMES[:9], base.FEATURE_NAMES)
        self.assertEqual(len(FEATURE_NAMES), 15)
        for sample_rate in (44_100, 48_000):
            with self.subTest(sample_rate=sample_rate):
                rng = np.random.default_rng(31)
                samples = rng.normal(0.0, 0.01, sample_rate // 2)
                samples[4000:4512] += np.hanning(512) * 0.8
                expected = base.fusion_features(samples, sample_rate)
                actual = fusion_features(samples, sample_rate)
                self.assertGreater(len(actual[0]), 0)
                np.testing.assert_array_equal(actual[0], expected[0])
                np.testing.assert_array_equal(actual[1], expected[1])
                self.assertEqual(actual[3], expected[3])
                self.assertEqual(actual[2][:, :9].tobytes(), expected[2].tobytes())
                added = actual[2][:, 9:]
                self.assertTrue(np.all(np.isfinite(added)))
                self.assertTrue(np.all((added[:, ::2] >= 0) & (added[:, ::2] <= 1)))
                self.assertTrue(np.all((added[:, 1::2] >= -6) & (added[:, 1::2] <= 6)))

    def test_prefix_invariance_at_deadline(self):
        rng = np.random.default_rng(6)
        prefix = rng.normal(0.0, 0.03, 12_345)
        suffix = rng.normal(0.0, 0.5, 7_891)
        short = fusion_features(prefix, 48_000)
        full = fusion_features(np.concatenate((prefix, suffix)), 48_000)
        keep = full[1] < len(prefix) // short[3]
        self.assertGreater(len(short[0]), 0)
        np.testing.assert_array_equal(short[0], full[0][keep])
        np.testing.assert_array_equal(short[1], full[1][keep])
        np.testing.assert_array_equal(short[2], full[2][keep])
        self.assertTrue(np.all(short[1] - short[0] == 8))

    def test_silence_and_short_input(self):
        for length in (0, 1, 255, 256, 4096):
            with self.subTest(length=length):
                candidates, available, features, hop = fusion_features(np.zeros(length), 48_000)
                self.assertEqual(hop, 256)
                self.assertEqual(candidates.shape, (0,))
                self.assertEqual(available.shape, (0,))
                self.assertEqual(features.shape, (0, 15))
        frequencies = self._frequencies()
        silent = self._reduce(np.zeros((12, len(frequencies))))
        np.testing.assert_array_equal(silent, np.zeros((1, 6)))

    @staticmethod
    def _frequencies():
        frequencies = np.fft.rfftfreq(2048, 1.0 / 48_000)
        return frequencies[(frequencies >= 30.0) & (frequencies <= 8000.0)]

    @staticmethod
    def _reduce(spectra):
        return _bandwise_features(
            spectra, 48_000, np.array([2]), np.array([10]), 256
        )

    def test_local_frequency_descent_only_changes_its_band(self):
        frequencies = self._frequencies()
        for band_index, (low, high) in enumerate(BANDS):
            with self.subTest(band=band_index):
                bins = np.flatnonzero((frequencies >= low) & (frequencies < high))
                spectra = np.zeros((12, len(frequencies)))
                # Equal-amplitude spectral lines move down within one band.
                # The first/last averaging windows see stable endpoints.
                spectra[:6, bins[-1]] = 1.0
                spectra[6:, bins[0]] = 1.0
                row = self._reduce(spectra)[0]
                expected = np.zeros(6)
                expected[band_index * 2] = 1.0 / (1.0 + base.EPSILON)
                expected[band_index * 2 + 1] = (
                    np.log2(frequencies[bins[-1]] / frequencies[bins[0]])
                    / (1.0 + base.EPSILON)
                )
                np.testing.assert_allclose(row, expected, atol=1e-12)
                # Reversing the movement preserves flux and negates the drop.
                reverse = self._reduce(spectra[::-1])[0]
                expected[band_index * 2 + 1] *= -1
                np.testing.assert_allclose(reverse, expected, atol=1e-12)

    def test_late_local_rise_is_included_until_but_not_after_deadline(self):
        frequencies = self._frequencies()
        for band_index, (low, high) in enumerate(BANDS):
            with self.subTest(band=band_index):
                selected = np.flatnonzero((frequencies >= low) & (frequencies < high))[0]
                spectra = np.zeros((12, len(frequencies)))
                spectra[:, selected] = 1.0
                unchanged = self._reduce(spectra)[0]
                np.testing.assert_array_equal(unchanged, np.zeros(6))
                spectra[10:, selected] = 4.0
                late = self._reduce(spectra)[0]
                self.assertAlmostEqual(late[band_index * 2], 0.75, places=11)
                other = np.delete(late, (band_index * 2, band_index * 2 + 1))
                np.testing.assert_array_equal(other, np.zeros(4))
                spectra[10, selected] = 1.0
                np.testing.assert_array_equal(self._reduce(spectra)[0], unchanged)


if __name__ == "__main__":
    unittest.main()
