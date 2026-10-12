#!/usr/bin/env python3
"""Remix renderer tests (synthetic audio only; no music folder). Run directly: test_remix.py"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

from tools.audio_analysis.catalog import catalog  # noqa: E402
from tools.audio_analysis.lab import remix  # noqa: E402

SR = 48000


def parts(n=SR * 3, seed=0):
    r = np.random.default_rng(seed)
    t = np.arange(n) / SR
    sigs = {'kick': ('kick', .8 * np.sin(2 * np.pi * 55 * t) * (t % .5 < .1)),
            'drums': ('drums', .3 * r.standard_normal(n) * (t % .25 < .02)),
            'bass': ('bass', .4 * np.sin(2 * np.pi * 70 * t)),
            'lead': ('other', .3 * np.sin(2 * np.pi * 880 * t)),
            'pad': ('other', .2 * np.sin(2 * np.pi * 330 * t))}
    return [remix.Part(k, role, (lambda a=a: a)) for k, (role, a) in sigs.items()], n


class RemixTests(unittest.TestCase):
    def test_same_seed_same_audio(self):
        ps, n = parts()
        a, pa = remix.render_parts(ps, n, SR, 7)
        b, pb = remix.render_parts(ps, n, SR, 7)
        self.assertTrue(np.array_equal(a, b))
        self.assertEqual(pa, pb)
        c, _ = remix.render_parts(ps, n, SR, 8)
        self.assertFalse(np.array_equal(a, c))

    def test_unity_is_the_plain_sum(self):
        ps, n = parts()
        out, params = remix.render_parts(ps, n, SR, 3, unity=True)
        want = sum(p.load() for p in ps)
        self.assertTrue(np.array_equal(out, want))
        self.assertIsNone(params['chain'])
        self.assertTrue(all(g == 0.0 for g in params['gains_db'].values()))

    def test_limiter_ceiling_holds(self):
        r = np.random.default_rng(1)
        x = 3.0 * r.standard_normal(SR * 2)
        x[1000] = 40.0
        y = remix.limit(x, SR)
        self.assertLessEqual(float(np.abs(y).max()), 10 ** (remix.CEILING_DB / 20) + 1e-12)
        quiet = .1 * r.standard_normal(SR)
        self.assertTrue(np.allclose(remix.limit(quiet, SR), quiet))        # untouched under the ceiling
        for seed in range(6):
            ps, n = parts()
            out, _ = remix.render_parts(ps, n, SR, seed)
            self.assertLessEqual(float(np.abs(out).max()), 10 ** (remix.CEILING_DB / 20) + 1e-9)

    def test_draws_stay_in_range_and_ignore_other_stems(self):
        g1, c1 = remix.draw(5, ['bass', 'drums', 'other:a'])
        g2, c2 = remix.draw(5, ['bass', 'drums', 'other:a', 'other:b', 'kick'])
        for g in g1:
            self.assertEqual(g1[g], g2[g])
        self.assertEqual(c1, c2)
        for s in range(50):
            gains, c = remix.draw(s, ['kick', 'other:x'])
            self.assertTrue(all(-6 <= v <= 6 for v in gains.values()))
            self.assertTrue(-3 <= c['low_db'] <= 3 and -3 <= c['high_db'] <= 3)
            self.assertTrue(1.5 <= c['ratio'] <= 4 and -4 <= c['thr_off_db'] <= 4)
            self.assertTrue(.005 <= c['attack_s'] <= .03 and .05 <= c['release_s'] <= .3 and 0 <= c['drive_db'] <= 6)

    def test_compressor_reduces_loud_parts_only(self):
        t = np.arange(SR * 2) / SR
        x = np.sin(2 * np.pi * 200 * t) * np.where(t < 1, .05, .8)
        y = remix.compress(x, SR, -26.0, 4.0, .005, .1)
        self.assertLess(float(np.abs(y[SR + 4000:]).max()), .8 * .7)
        self.assertAlmostEqual(float(np.abs(y[:SR - 4000]).max()), .05, delta=.002)

    def test_shift_aligns(self):
        x = np.arange(10.0)
        self.assertTrue(np.array_equal(remix.shift(x, 3, 10), np.r_[0, 0, 0, np.arange(7.0)]))
        self.assertTrue(np.array_equal(remix.shift(x, -3, 10), np.r_[np.arange(3.0, 10.0), 0, 0, 0]))

    def test_roles_group_for_sample_filenames(self):
        names = {'Late Night - Drums Stem.wav': 'drums', 'Late Night - Kicks Stem.wav': 'kick',
                 'Late Night - Bass and Sub Stem.wav': 'bass', 'Late Night - Leads Stem.wav': 'other',
                 'Late Night - Vocal Stem.wav': 'other', 'Drums snare 1.wav': 'drums', 'Drums HH.wav': 'drums',
                 'Drums kick.wav': 'kick', '_ HATS.wav': 'drums', '_ SNARE.wav': 'drums', 'Subs.wav': 'bass',
                 'Corrosion - PREMASTER - NO DRUMS.wav': 'nodrums', 'Corrosion - PREMASTER - DRUMS.wav': 'drums',
                 'Pattern - NO KICK - 32Bit.wav': 'nokick', 'Pattern - KICK STEM - 32Bit.wav': 'kick',
                 'Chords.wav': 'other', 'Worship Stems - MM_jungleclap-24b.wav': 'drums'}
        for name, want in names.items():
            role, _ = catalog.classify(name, ['STEMS'], True)
            self.assertEqual(catalog.REMIX_ROLE.get(role, role), want, name)
        ps = [remix.Part(n, 'other', lambda: np.zeros(1)) for n in ('a', 'b')] + [remix.Part('k', 'kick', lambda: np.zeros(1))]
        self.assertEqual(sorted({remix.group_of(p) for p in ps}), ['kick', 'other:a', 'other:b'])


if __name__ == '__main__':
    unittest.main()
