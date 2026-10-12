#!/usr/bin/env python3
"""The coincident-hit score's tests (hand-built two-song fixture; no audio, no GPU). Run directly: test_coincident.py"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.lab import compare, config, recipe  # noqa: E402

recipe.apply(config.load('snare_base').net_env())

import numpy as np  # noqa: E402

from tools.audio_analysis.lab import score  # noqa: E402

SR, HOP = 48000, 256


def labels(positives):
    return dict(positives=positives, hard_neg=[], unscored=[], loop_spans=[], duration_s=10.0, positives_only=False)


# Song a: snare labels 1, 2, 3, 4 against kicks at 1.010 (10 ms: together), 2.025 (25 ms: not), 3.000 (together),
# 5.000. Song b: snare labels 1, 2 against kicks at 2.020 (exactly 20 ms: together) and 2.500.
SNARE = dict(a=labels([1.0, 2.0, 3.0, 4.0]), b=labels([1.0, 2.0]))
KICK = dict(a=labels([1.010, 2.025, 3.0, 5.0]), b=labels([2.020, 2.5]))
# The detector fires on a's 1, 3 and 4 and misses a's 2; on b it fires on 1 and misses 2.
FIRES = dict(a=[1.0, 3.0, 4.0], b=[1.0])


def fixture(t, fire):
    """(stamp, p, avail) for candidates at every label of the song: p .9 where it fires, .1 elsewhere."""
    stamp = np.array(SNARE[t]['positives'])
    avail = np.round(stamp * SR / HOP).astype(int) + 1
    p = np.array([.9 if any(abs(s - f) < 1e-9 for f in fire) else .1 for s in stamp])
    return stamp, p, avail


class CoincidentTests(unittest.TestCase):
    def test_coincident_set_is_within_20_ms_of_the_other_family(self):
        for t, snares, kicks in (('a', [1.0, 3.0], [1.010, 3.0]), ('b', [2.0], [2.020])):
            m = score.coincident_mask(SNARE[t]['positives'], KICK[t]['positives'])
            self.assertEqual([x for x, h in zip(SNARE[t]['positives'], m) if h], snares)
            # The mirror: kick labels within 20 ms of a snare label.
            m = score.coincident_mask(KICK[t]['positives'], SNARE[t]['positives'])
            self.assertEqual([x for x, h in zip(KICK[t]['positives'], m) if h], kicks)
        self.assertEqual(len(score.coincident_mask([], [1.0])), 0)
        self.assertEqual(score.coincident_mask([1.0], []).tolist(), [False])

    def test_recall_on_the_coincident_set_per_song_and_pooled(self):
        co = {}
        for t in 'ab':
            stamp, p, avail = fixture(t, FIRES[t])
            co[t] = score.coincident_result(SNARE[t], KICK[t]['positives'], stamp, p, avail, .5, SR, HOP)
        self.assertEqual(co['a'], dict(labels=4, matched=3, coincident_labels=2, coincident_matched=2))
        self.assertEqual(co['b'], dict(labels=2, matched=1, coincident_labels=1, coincident_matched=0))
        pooled = score.coincident_pooled(co, ['a', 'b'])
        self.assertEqual((pooled['coincident_labels'], pooled['coincident_matched']), (3, 2))
        self.assertAlmostEqual(pooled['coincident_recall'], 2 / 3)
        self.assertAlmostEqual(pooled['recall'], 4 / 6)   # overall recall is not the coincident one
        self.assertIsNone(score.coincident_pooled(co, [])['recall'])

    def test_overall_matches_the_scorers_own_count(self):
        """The overall counts beside the coincident ones are the numbers the lab scorer reports for the song."""
        from tools.audio_analysis.eval.detector_eval import song_result
        for t in 'ab':
            stamp, p, avail = fixture(t, FIRES[t])
            r = song_result(SNARE[t], stamp, p, avail, .5, SR, 0.0, HOP)
            c = score.coincident_result(SNARE[t], KICK[t]['positives'], stamp, p, avail, .5, SR, HOP)
            self.assertEqual((c['labels'], c['matched']), (r['labels'], r['matched']))

    def test_tight_window_family_judged_by_its_own_rule(self):
        s = dict(SNARE['a'], window_s=[-.030, .030])
        stamp, p, avail = fixture('a', FIRES['a'])
        c = score.coincident_result(s, KICK['a']['positives'], stamp + .020, p, avail, .5, SR, HOP)
        self.assertEqual((c['matched'], c['coincident_matched']), (3, 2))
        c = score.coincident_result(s, KICK['a']['positives'], stamp + .040, p, avail, .5, SR, HOP)
        self.assertEqual((c['matched'], c['coincident_matched']), (0, 0))

    def test_section_and_partner(self):
        co = {'a': dict(labels=4, matched=3, coincident_labels=2, coincident_matched=2),
              'gen': dict(labels=2, matched=2, coincident_labels=1, coincident_matched=1)}
        sec = score.coincident_section('snare', co, dict(exam=['a', 'x']))
        self.assertEqual((sec['partner'], sec['window_ms']), ('kick', 20))
        self.assertEqual(sec['pooled']['songs'], 1)          # only exam songs pool; 'gen' is listed per song
        self.assertEqual(sorted(sec['per_song']), ['a', 'gen'])
        self.assertEqual(score.coincident_section('kick', co, dict(exam=['a']))['partner'], 'snare')
        self.assertIsNone(score.coincident_section('bass', co, dict(exam=['a'])))
        self.assertIsNone(score.coincident_section('snare', {}, dict(exam=['a'])))

    def test_compare_text_per_song_and_pooled(self):
        def rep(matched_a, matched_b):
            co = dict(a=dict(labels=4, matched=3, coincident_labels=2, coincident_matched=matched_a),
                      b=dict(labels=2, matched=1, coincident_labels=1, coincident_matched=matched_b),
                      n=dict(labels=5, matched=5, coincident_labels=0, coincident_matched=0))
            return dict(exam=['a', 'b', 'n'], coincident=score.coincident_section('snare', co, dict(exam=['a', 'b', 'n'])))
        lines = compare.coincident_lines([[rep(2, 0)], [rep(1, 1)]], 'snare')
        self.assertIn('snare labels within 20 ms of a kick label', lines[0])
        self.assertIn('A: coincident 0.667 (2.0 of 3)', lines[1])
        self.assertIn('overall 0.818 (9.0 of 11)', lines[1])
        self.assertIn('B: coincident 0.667 (2.0 of 3)', lines[2])
        song_a = next(ln for ln in lines if ln.lstrip().startswith('a '))
        self.assertIn('2.0 /   1.0 (of   2)', song_a)
        self.assertFalse(any(ln.lstrip().startswith('n ') for ln in lines))   # no coincident labels: no row
        # A report from before the score has nothing to compare.
        self.assertEqual(compare.coincident_lines([[dict(rep(2, 0), coincident=None)], [rep(1, 1)]], 'snare'), [])


if __name__ == '__main__':
    unittest.main()
