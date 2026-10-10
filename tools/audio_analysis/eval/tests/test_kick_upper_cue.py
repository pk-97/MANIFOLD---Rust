import unittest

import numpy as np

from eval.kick_upper_cue import inspect_cues, upper_features


SR = 48_000
HOP = 256


def _envelopes(length=20):
    return np.zeros((length, 4, 2), dtype=float)


def _upper(env, start, end=None, value=3.0):
    end = len(env) if end is None else end
    env[start:end, 1:, 0] = value
    env[start:end, 1:, 1] = 1.0


def _low(env, start, end=None):
    end = len(env) if end is None else end
    env[start:end, 0, 0] = 2.0
    env[start:end, 0, 1] = 1.0


class KickUpperCueTests(unittest.TestCase):
    def test_paired_cue_passes_decay_and_linkage(self):
        env = _envelopes(20)
        _upper(env, 2, 4)
        _low(env, 2, 3)
        rows = inspect_cues(env, SR, HOP)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["upper_hop"], 2)
        self.assertEqual(rows[0]["low_hop"], 2)
        self.assertTrue(rows[0]["linked_and_decayed"])
        self.assertLessEqual(rows[0]["upper_decay_ratio"], 0.5)

    def test_upper_only_event_fails_linkage(self):
        env = _envelopes(20)
        _upper(env, 2, 4)
        rows = inspect_cues(env, SR, HOP)
        self.assertEqual(rows[0]["low_hop"], None)
        self.assertFalse(rows[0]["linked_and_decayed"])

    def test_steady_low_plus_fresh_upper_fails_linkage(self):
        env = _envelopes(20)
        _low(env, 0, 10)
        _upper(env, 4, 6)
        rows = inspect_cues(env, SR, HOP)
        self.assertEqual(rows[0]["low_hop"], None)
        self.assertFalse(rows[0]["low_linked"])

    def test_low_onset_after_thirty_five_ms_fails_linkage(self):
        env = _envelopes(30)
        _upper(env, 2, 4)
        _low(env, 10, 11)
        rows = inspect_cues(env, SR, HOP)
        self.assertEqual(rows[0]["low_hop"], None)

    def test_sustained_upper_fails_decay(self):
        env = _envelopes(20)
        _upper(env, 2, 20)
        rows = inspect_cues(env, SR, HOP)
        self.assertEqual(len(rows), 1)
        self.assertFalse(rows[0]["decayed"])
        self.assertFalse(rows[0]["linked_and_decayed"])

    def test_features_are_prefix_invariant(self):
        rng = np.random.default_rng(5)
        prefix = rng.normal(size=SR // 3)
        suffix = rng.normal(size=SR // 4)
        short, short_hop = upper_features(prefix, SR)
        full, full_hop = upper_features(np.concatenate((prefix, suffix)), SR)
        self.assertEqual(short_hop, full_hop)
        np.testing.assert_array_equal(short, full[: len(short)])

    def test_cue_prefix_does_not_deliver_future_deadlines(self):
        env = _envelopes(30)
        _upper(env, 2, 4)
        _low(env, 2, 3)
        _upper(env, 12, 14)
        short = inspect_cues(env[:10], SR, HOP)
        full = inspect_cues(env, SR, HOP)
        self.assertEqual(short, [row for row in full if row["available_hop"] < 10])


if __name__ == "__main__":
    unittest.main()
