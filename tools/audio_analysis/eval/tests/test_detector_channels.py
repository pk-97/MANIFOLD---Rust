"""Noisiness and pitch-stability channels: noise reads as noise, a held tone as pitched, and both are causal."""
import unittest

import numpy as np

from eval.detector_channels import flatness, pitch_stability
from eval.kick_goal_nn import CENTRES

SR = 48000


class DetectorChannelTests(unittest.TestCase):
    def setUp(self):
        t = np.arange(SR) / SR
        self.noise = np.random.default_rng(0).standard_normal(SR)
        self.saw = 2 * ((220 * t) % 1) - 1

    def test_noise_is_flat_and_a_saw_is_peaky(self):
        bands = [int(np.argmin(np.abs(CENTRES - f))) for f in (1000, 2000, 4000)]
        noise = flatness(self.noise, SR)[100:, bands].mean(0)
        saw = flatness(self.saw, SR)[100:, bands].mean(0)
        self.assertTrue(np.all(noise > -4), noise)  # a periodogram of white noise sits near -2.5 dB
        self.assertTrue(np.all(saw < -10), saw)

    def test_held_pitch_is_stable_and_noise_is_not(self):
        self.assertTrue(np.all(pitch_stability(self.saw, SR)[100:].mean(0) > .8))
        # One second of noise averages 400 frames: it wanders by about 0.1 around zero.
        self.assertTrue(np.all(np.abs(pitch_stability(self.noise, SR)[100:].mean(0)) < .2))

    def test_channels_are_causal(self):
        # Changing audio after a frame must not change that frame.
        y = self.noise.copy()
        y[SR // 2:] = 0
        n = SR // 2 // 96  # frames ending at or before the change
        np.testing.assert_array_equal(flatness(self.noise, SR)[:n], flatness(y, SR)[:n])
        np.testing.assert_array_equal(pitch_stability(self.noise, SR)[:n], pitch_stability(y, SR)[:n])


if __name__ == '__main__':
    unittest.main()
