"""Shared detector scoring and candidates: refractory fires, what counts as false, cutoff choice, causal rises."""
import unittest

import numpy as np

from eval.detector_cands import HOP, band_hops, rise_candidates
from eval.detector_eval import choose, fires, rows, score

SR = 48000


def labels(pos, unscored=(), loops=(), hard=(), only=False):
    return dict(positives=list(pos), unscored=[list(u) for u in unscored], loop_spans=[list(u) for u in loops],
                hard_neg=list(hard), positives_only=only)


def hop_of(t):
    return np.round(np.asarray(t) * SR / HOP).astype(int) - 1


class ScoringTests(unittest.TestCase):
    def test_refractory_keeps_the_first_of_close_fires(self):
        avail = hop_of([1.0, 1.03, 1.2])
        self.assertEqual(list(fires(np.ones(3), avail, .5, SR)), [0, 2])

    def test_false_fires_skip_unscored_and_unvouched_loop_hits(self):
        s = labels([1.0], unscored=[(2.0, 2.2)], loops=[(3.0, 4.0)], hard=[3.5])
        emit = np.array([1.02, 2.1, 3.2, 3.5, 5.0])
        m, n, f = score(s, emit, np.ones(5), hop_of(emit), .5, SR)
        # caught 1.0; false: 3.5 (vouched negative in a loop) and 5.0; 2.1 is unscored, 3.2 is an unvouched loop hit.
        self.assertEqual((m, n, f), (1, 1, 2))

    def test_recall_only_songs_never_count_false_fires(self):
        s = labels([1.0], only=True)
        emit = np.array([1.0, 2.0])
        self.assertEqual(score(s, emit, np.ones(2), hop_of(emit), .5, SR), (1, 1, 0))
        mask, y, _ = rows(s, emit)
        self.assertEqual(list(mask), [True, False])

    def test_precision_cutoff_trades_recall_for_precision(self):
        s = labels([1.0, 2.0, 3.0])
        emit = np.array([1.0, 2.0, 3.0, 4.0, 5.0])
        p = np.array([.9, .9, .3, .5, .2])
        items = [(s, emit, p, hop_of(emit), SR)]
        self.assertLess(choose(items), .3 + 1e-9)            # best F1 keeps the .3 snare and pays one false fire
        self.assertGreater(choose(items, min_precision=1.0), .5)  # perfect precision drops both


class CandidateTests(unittest.TestCase):
    def test_a_burst_gives_one_candidate_at_its_attack_and_is_causal(self):
        x = np.zeros(SR)
        x[SR // 2:SR // 2 + 2400] = np.random.default_rng(0).standard_normal(2400)
        e = band_hops(x, SR, 1000, 8000)
        cand, avail = rise_candidates(e, 4.5, 4, 6, 8)
        self.assertEqual(len(cand), 1)
        self.assertLessEqual(abs(int(cand[0]) - (SR // 2) // HOP), 1)
        self.assertEqual(int(avail[0]), int(cand[0]) + 8)
        y = x.copy()
        y[SR // 2 + 4800:] = 1.0  # a later change cannot move an earlier hop
        np.testing.assert_array_equal(band_hops(y, SR, 1000, 8000)[:cand[0] + 1], e[:cand[0] + 1])


if __name__ == '__main__':
    unittest.main()
