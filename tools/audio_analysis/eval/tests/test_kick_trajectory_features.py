import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np
from scipy.io import wavfile

from eval.kick_attack_rejection import sha
from eval.kick_fusion_bandwise import fusion_features as base
from eval.kick_trajectory_features import cached_features, fusion_features, trajectory_rows


class TrajectoryTests(unittest.TestCase):
    def test_order_is_retained_with_pre_candidate_background(self):
        fast = np.ones((12, 3)); slow = np.ones_like(fast)
        fast[3:11, 0] = np.arange(1, 9)
        fast[3:11, 1] = np.arange(8, 0, -1)
        slow[2:] = 99  # After the candidate starts, background changes are irrelevant.
        result = trajectory_rows(fast, slow, np.array([2]), np.array([10]))[0]
        np.testing.assert_allclose(result[:8], np.log(np.arange(1, 9)), atol=1e-11)
        np.testing.assert_allclose(result[8:16], np.log(np.arange(8, 0, -1)), atol=1e-11)
        np.testing.assert_array_equal(result[16:], np.zeros(8))

    def test_frozen_features_and_grid_preserved(self):
        for sr in (44100, 48000):
            audio = np.random.default_rng(21).normal(0, .02, sr//3)
            old, new = base(audio, sr), fusion_features(audio, sr)
            self.assertGreater(len(new[0]), 0)
            for i in (0, 1): np.testing.assert_array_equal(old[i], new[i])
            np.testing.assert_array_equal(old[2], new[2][:, :15])
            self.assertEqual(new[2].shape[1], 39)
            self.assertTrue(np.isfinite(new[2]).all())

    def test_prefix_invariance_and_no_added_horizon(self):
        prefix = np.random.default_rng(11).normal(0, .02, 12900)
        suffix = np.random.default_rng(12).normal(0, .2, 5900)
        short, long = fusion_features(prefix, 48000), fusion_features(np.r_[prefix, suffix], 48000)
        keep = long[1] < len(prefix)//256
        for i in (0, 1, 2): np.testing.assert_array_equal(short[i], long[i][keep])
        self.assertTrue(np.all(short[1]-short[0] == 8))

    def test_silence_and_short_input(self):
        for n in (0, 200, 4096):
            self.assertEqual(fusion_features(np.zeros(n), 48000)[2].shape, (0, 39))

    def test_cache_reuses_without_decoding_and_rejects_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'audio.wav'
            wavfile.write(path, 48000, np.zeros(4096, dtype=np.float32))
            digest = sha(path)
            first = cached_features(path, digest, Path(directory)/'cache')
            with patch('eval.kick_trajectory_features.read_audio', side_effect=AssertionError('decoded')):
                second = cached_features(path, digest, Path(directory)/'cache')
            self.assertTrue(second['cache_hit'])
            np.testing.assert_array_equal(first['features'], second['features'])
            Path(first['metadata']['data_path']).write_bytes(b'broken')
            with self.assertRaisesRegex(ValueError, 'provenance'):
                cached_features(path, digest, Path(directory)/'cache')


if __name__ == '__main__': unittest.main()
