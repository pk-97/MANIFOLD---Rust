"""Focused algebraic and causal checks for the stem-mixture diagnostic."""
import unittest

import numpy as np

from .kick_controlled_mixtures import construct_conditions, fixed_anchor_features, source_anchor
from .kick_fusion_bandwise import fusion_features


class ControlledMixturesTests(unittest.TestCase):
    def test_addition_removal_identity_and_common_gain(self):
        kick = np.array([.8, -.6, .2, 0.0])
        other = np.array([.6, .7, -.2, 0.0])
        common, conditions = construct_conditions(kick, other)
        self.assertAlmostEqual(common, .95/2)
        for gain in (0, 1, 2):
            np.testing.assert_allclose(conditions[gain, True]-conditions[gain, False], common*kick, atol=1e-16)
            np.testing.assert_array_equal(conditions[gain, False], common*gain*other)
        self.assertAlmostEqual(max(np.max(np.abs(x)) for x in conditions.values()), .95)
        np.testing.assert_array_equal(conditions[0, False], np.zeros(4))

    def test_no_unrequested_normalisation(self):
        common, conditions = construct_conditions(np.array([.02, .01]), np.array([.01, .02]))
        self.assertEqual(common, 1.0)
        np.testing.assert_array_equal(conditions[1, True], [.03, .03])

    def test_shape_and_finite_guard(self):
        for kick, other in [(np.zeros(2), np.zeros(3)), (np.array([np.nan]), np.zeros(1))]:
            with self.assertRaises(ValueError):
                construct_conditions(kick, other)

    def test_anchor_rejects_previous_tail_outside_reviewed_bracket(self):
        kick = np.zeros(48000)
        kick[round(.45*48000):round(.47*48000)] = .2
        kick[round(.498*48000):round(.51*48000)] = .8
        sample, _ = source_anchor(kick, .5, .485)
        self.assertEqual(sample, round(.498*48000))

    def test_completed_deadline_prefix_is_causal(self):
        sr, hop = 48000, 256
        t = np.arange(sr)/sr
        samples = .01*np.sin(2*np.pi*80*t)
        u = np.maximum(0, t-.4)
        samples += (t>=.4)*np.exp(-u/.03)*np.sin(2*np.pi*(170*u-80*u*u))
        anchor = int(.4*sr)//hop
        available, features, _ = fixed_anchor_features(samples, sr, anchor)
        deadline_samples = (int(available[0])+1)*hop
        prefix = samples[:deadline_samples]
        p_available, p_features, _ = fixed_anchor_features(prefix, sr, anchor)
        np.testing.assert_array_equal(available, p_available)
        np.testing.assert_allclose(features, p_features, atol=1e-13, rtol=0)
        candidates, deadlines, natural, _ = fusion_features(samples, sr)
        p_candidates, p_deadlines, p_natural, _ = fusion_features(prefix, sr)
        keep = deadlines < deadline_samples//hop
        np.testing.assert_array_equal(p_candidates, candidates[keep])
        np.testing.assert_array_equal(p_deadlines, deadlines[keep])
        np.testing.assert_allclose(p_natural, natural[keep], atol=1e-13, rtol=0)


if __name__ == '__main__':
    unittest.main()
