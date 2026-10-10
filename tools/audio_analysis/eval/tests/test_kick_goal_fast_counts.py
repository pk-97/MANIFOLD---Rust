#!/usr/bin/env python3
"""fast_fires/fast_counts equal fires()/counts(score()) on synthetic songs: whole songs with regions, passages."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.audio_analysis.eval.kick_goal_eval import counts, fast_counts, fast_fires, greedy_matches, score  # noqa: E402
from tools.audio_analysis.eval.live_kick_baseline import match_events  # noqa: E402

SR, HOP = 48000, 256


def record(source, n, rng):
    available = np.sort(rng.integers(0, 30 * SR // HOP, n))
    return dict(source=source, available=available, sample_rate=SR, hop=HOP, emit_s=(available + 1) * HOP / SR)


def songs(rng):
    """A whole song with uncertain regions and a two-passage song, both with dense candidates."""
    grid = np.round(np.arange(1.5, 28, .43) + rng.uniform(-.02, .02, 62), 3)
    regions = [dict(start_s=5.0, end_s=6.0), dict(start_s=20.0, end_s=20.5)]
    truth = [float(x) for x in grid if not any(r['start_s'] <= x <= r['end_s'] for r in regions)]
    whole = dict(track='whole', group='original_five', truth=truth, regions=regions)
    passages = [dict(id='a', start_s=3.0, end_s=12.0, review_start_s=2.0, review_end_s=13.0,
                     kick_times_s=[float(x) for x in grid if 2.0 <= x <= 13.0],
                     uncertain_regions=[dict(start_s=7.0, end_s=7.3, reason='x')]),
                dict(id='b', start_s=15.0, end_s=25.0, review_start_s=14.5, review_end_s=25.5,
                     kick_times_s=[float(x) for x in grid if 14.5 <= x <= 25.5])]
    return SimpleNamespace(records={'whole': record(whole, 3000, rng),
                                    'passages': record(dict(group='new_masters', passages=passages), 3000, rng)})


class FastCountTests(unittest.TestCase):
    def test_greedy_count_equals_exact_matcher(self):
        rng = np.random.default_rng(7)
        cases = [([], []), ([1.0], []), ([], [1.0]), ([1.07], [1.0]), ([0.93], [1.0]), ([1.0 + .07 + 2e-9], [1.0]),
                 ([1.04, 1.08], [1.0, 1.05]), (list(np.arange(0, 2, .01)), list(np.arange(0, 2, .05)))]
        for _ in range(300):
            # Times on a 5 ms grid make exact tolerance-edge differences common.
            cases.append((sorted(set(rng.integers(0, 400, rng.integers(0, 40)) * .005)),
                          sorted(set(rng.integers(0, 400, rng.integers(0, 25)) * .005))))
        for pred, truth in cases:
            for tol in (.035, .05, .07):
                self.assertEqual(greedy_matches(pred, truth, -tol - 1e-9, tol + 1e-9),
                                 len(match_events(pred, truth, tol, tol)), (pred, truth, tol))

    def test_fast_counts_equal_report_counts(self):
        rng = np.random.default_rng(3)
        g = songs(rng)
        for t, r in g.records.items():
            for p in (rng.uniform(0, 1, len(r['available'])), rng.beta(.3, .3, len(r['available']))):
                for th in np.concatenate([[0.0, 1.0, 2.0], np.quantile(p, np.linspace(0, 1, 41))]):
                    idx, passages = score(g, t, p, th)
                    self.assertTrue(np.array_equal(idx, fast_fires(g, t, p, th)))
                    for ms in ('35', '50', '70'):
                        self.assertEqual(counts(passages, ms), fast_counts(g, t, p, th, ms), (t, th, ms))


if __name__ == '__main__':
    unittest.main()
