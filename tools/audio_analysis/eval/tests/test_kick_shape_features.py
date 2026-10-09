import unittest

import numpy as np

from eval.kick_shape_features import FEATURE_NAMES, from_cached_features, shape_rows


class ShapeFeatureTests(unittest.TestCase):
    def test_flat_bands_are_neutral_and_empty_input_is_supported(self):
        np.testing.assert_array_equal(shape_rows(np.zeros((2, 3, 8))), np.zeros((2, 9)))
        self.assertEqual(from_cached_features(np.empty((0, 39))).shape, (0, len(FEATURE_NAMES)))

    def test_temporal_mass_matches_hand_calculation(self):
        power = np.ones((1, 3, 8))
        power[0, 0, 7] = 9
        power[0, 1, 0] = 9
        result = shape_rows(np.log(power))[0]
        # Low/body centres are .75/.25; the upper band is uniform at .5.
        np.testing.assert_allclose(result[:3], [.5, .25, -.25], atol=1e-14)
        self.assertAlmostEqual(result[3], -1 / 7, places=10)
        np.testing.assert_array_equal(result[4:6], [0., 0.])
        np.testing.assert_allclose(result[6:], [.25, .25, 0.], atol=1e-14)

    def test_independent_band_scale_offsets_do_not_change_shape(self):
        x = np.random.default_rng(3).normal(size=(7, 3, 8))
        shift = np.array([100., -100., 2.])[None, :, None]
        np.testing.assert_allclose(shape_rows(x), shape_rows(x + shift), atol=1e-13)

    def test_reversal_changes_timing_direction_only(self):
        x = np.random.default_rng(7).normal(size=(5, 3, 8))
        before, after = shape_rows(x), shape_rows(x[:, :, ::-1])
        np.testing.assert_allclose(before[:, :3], -after[:, :3], atol=1e-14)
        np.testing.assert_allclose(before[:, 3:], after[:, 3:], atol=1e-14)

    def test_pair_agreement_requires_both_bands_to_vary(self):
        x = np.zeros((1, 3, 8))
        x[0, :2, 3] = 8
        result = shape_rows(x)[0]
        self.assertGreater(result[3], .999999)
        np.testing.assert_array_equal(result[4:6], [0., 0.])

    def test_base_columns_exact_and_rows_independent(self):
        x = np.random.default_rng(5).normal(size=(6, 39))
        out = from_cached_features(x)
        np.testing.assert_array_equal(out[:, :15], x[:, :15])
        for i in range(len(x)):
            np.testing.assert_array_equal(out[i:i+1], from_cached_features(x[i:i+1]))
        self.assertEqual(out.shape, (6, 24))
        self.assertTrue(np.all(np.abs(out[:, 15:21]) <= 1))
        self.assertTrue(np.all((out[:, 21:] >= 0) & (out[:, 21:] <= 1)))

    def test_invalid_input_is_rejected(self):
        for x in (np.zeros((2, 24)), np.full((1, 39), np.nan)):
            with self.assertRaises(ValueError):
                from_cached_features(x)


if __name__ == '__main__':
    unittest.main()
