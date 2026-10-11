#!/usr/bin/env python3
"""The lab's invariant tests (docs/DETECTOR_LAB_DESIGN.md section 4). Run directly: test_lab.py"""
import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.lab import config, recipe  # noqa: E402

BASE = config.load('snare_base')
recipe.apply(BASE.net_env())

import numpy as np  # noqa: E402

from tools.audio_analysis.lab import audit, cache, compare, models, run, score  # noqa: E402
from tools.audio_analysis.lab.registry import Song, load, sha256_file  # noqa: E402

GOLDEN = Path(__file__).with_name('tests') / 'golden' / 'legacy_snare_folds4_s0'


def song(id_, path):
    return Song(id=id_, kind='project', source=dict(how='wip', paths=[str(path)]), source_sha256=(sha256_file(path),),
                sample_rate=48000, offset_s=0.0, proof='wip:proven', families=('snare',))


class LabTests(unittest.TestCase):
    def test_recycled_name_misses_cache(self):
        """The same song id over different audio must rebuild, never reuse (the linn_* trap)."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            a, b = d / 'a.npy', d / 'b.npy'
            np.save(a, np.zeros(10, np.float32))
            np.save(b, np.ones(10, np.float32))
            builds = []

            def build(v):
                return lambda: builds.append(v) or np.full(3, v, np.float16)
            first = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=64), build(0), root=d / 'c')
            again = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=64), build(9), root=d / 'c')
            other = cache.artifact(song('linn_1', b), 'spectrum', dict(bands=64), build(1), root=d / 'c')
            setting = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=128), build(2), root=d / 'c')
            self.assertEqual(builds, [0, 1, 2])
            self.assertEqual((first[0], again[0], other[0], setting[0]), (0, 0, 1, 2))

    def test_recipe_conflict_exits(self):
        old = os.environ.get('KICK_GOAL_TRUTH')
        os.environ['KICK_GOAL_TRUTH'] = 'strict'
        try:
            with self.assertRaises(SystemExit):
                recipe.apply()
        finally:
            os.environ['KICK_GOAL_TRUTH'] = old if old is not None else 'v3'

    def test_audit_keeps_labels_sha(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'x_labels.json'
            p.write_text(json.dumps({'s': {'positives': [1.0]}}))
            before = hashlib.sha256(p.read_bytes()).hexdigest()
            side = audit.write_sidecar(p, {'s': {'pass': True}})
            self.assertEqual(hashlib.sha256(p.read_bytes()).hexdigest(), before)
            self.assertEqual(json.loads(side.read_text())['labels_sha256'], before)

    def test_cutoff_ignores_generated_and_recall_only(self):
        """Changing what a generated or recall-only song predicts never moves another song's cutoff."""
        from tools.audio_analysis.eval.detector_eval import held_out
        rng = np.random.default_rng(0)
        lab, meta, held = {}, {}, {}
        for k, (t, extra) in enumerate([('a', {}), ('b', {}), ('c', {}), ('gen', dict(kind='generated')),
                                        ('ro', dict(positives_only=True))]):
            pos = np.sort(rng.uniform(1, 59, 40))
            avail = np.sort(rng.integers(100, 11000, 300))
            lab[t] = dict(positives=list(pos), hard_neg=[], unscored=[], loop_spans=[], duration_s=60.0,
                          positives_only=extra.get('positives_only', False), **({'kind': 'generated'} if 'kind' in extra else {}))
            meta[t] = (lab[t], (avail + 1) * 256 / 48000, avail, 48000)
            held[t] = rng.uniform(0, 1, 300)

        def cutoffs(h):
            cut = {}
            held_out(lab, meta, h, log=None, cutoffs=cut)
            return cut
        before = cutoffs(held)
        after = cutoffs({**held, 'gen': rng.uniform(0, 1, 300) ** 4, 'ro': rng.uniform(0, 1, 300) ** .2})
        self.assertEqual(before, after)
        # The same change to an exam song does move them: the test can fail.
        self.assertNotEqual(before, cutoffs({**held, 'c': rng.uniform(0, 1, 300) ** 4}))

    def test_pool_matches_leave_one_out(self):
        """A pool holding every other song's own prediction picks the cutoffs the plain leave-one-out rule does, and a
        pool that differs moves them: the pool path is the rule, not a copy of it."""
        from tools.audio_analysis.eval.detector_eval import held_out
        rng = np.random.default_rng(1)
        lab, meta, held = {}, {}, {}
        for t in 'abcde':
            pos = np.sort(rng.uniform(1, 59, 40))
            avail = np.sort(rng.integers(100, 11000, 300))
            lab[t] = dict(positives=list(pos), hard_neg=[], unscored=[], loop_spans=[], duration_s=60.0,
                          positives_only=False)
            meta[t] = (lab[t], (avail + 1) * 256 / 48000, avail, 48000)
            held[t] = rng.uniform(0, 1, 300)

        def cutoffs(pool):
            cut = {}
            held_out(lab, meta, held, log=None, cutoffs=cut, pool=pool)
            return cut
        loo = {t: {u: held[u] for u in held if u != t} for t in held}
        self.assertEqual(cutoffs(None), cutoffs(loo))
        skew = {t: {u: p ** 4 for u, p in d.items()} for t, d in loo.items()}
        self.assertNotEqual(cutoffs(None), cutoffs(skew))

    def test_layer_saves_and_pools(self):
        """Saves read back per layer; group k's pool is the pair predictions made without k, one per other song."""
        cfg = config.build('x', dict(family='snare', candidates='snare:4.5', models=dict(trees=dict(
            measures=['tail3'], max_iter=10, learning_rate=.1, max_leaf_nodes=7, min_samples_leaf=5, l2=1.0)),
            combine='trees', stage=None, sampling=dict(per_song='median'), train=['project'], seeds=[0], folds=3))
        lab = {t: {} for t in 'abcdef'}
        songs = {t: type('S', (), dict(kind='project'))() for t in lab}
        grps, trainings, _ = models.plan(cfg, lab, songs)
        z = {}
        for i, n in enumerate(trainings):
            for t, pre in models.save_keys(grps, n).items():
                z[f'{pre}__trees__{t}' if pre else f'trees__{t}'] = np.array([float(i)])
        meta = dict(schema=2, songs=list(lab), groups=grps, trainings=trainings)
        held, pool = score.layer_of(meta, z, 'trees')
        for k, g in enumerate(grps):
            for t in g:
                self.assertEqual(float(held[t][0]), float(k))
                self.assertEqual(sorted(pool[t]), sorted(u for u in lab if u not in g))
                for u, v in pool[t].items():
                    n = trainings[int(v[0])]
                    self.assertEqual(sorted(n['without']), sorted({k, next(j for j, h in enumerate(grps) if u in h)}))
            self.assertTrue(all(pool[t] is pool[g[0]] for t in g))

    def test_config_schema(self):
        """Schema 1 configs migrate; typos, a combine that does not match the models, unknown parts and a stage are
        refused."""
        old = dict(family='snare', candidates='snare:4.5', net=dict(past_ms=190, ahead_ms=40, flat=True, epochs=12),
                   sampling=dict(per_song='median'), train=['project'], seeds=[0], folds=4)
        cfg = config.build('old', old)
        self.assertEqual(cfg.models, dict(net=dict(past_ms=190, ahead_ms=40, channels=['flat'], epochs=12)))
        self.assertEqual((cfg.combine, cfg.stage, cfg.ahead_s), ('net', None, .04))
        good = config.migrate(old)
        trees = dict(measures=['tail3'], max_iter=10, learning_rate=.1, max_leaf_nodes=7, min_samples_leaf=5, l2=1.0)
        config.build('both', dict(good, models=dict(good['models'], trees=trees), combine='mean_logit'))
        for bad in (dict(good, extra=1), dict(good, combine='trees'),
                    dict(good, models=dict(good['models'], trees=trees)),
                    dict(good, models=dict(trees=dict(trees, measures=['nope'])), combine='trees'),
                    dict(good, models=dict(net=dict(good['models']['net'], channels=['nope']))),
                    dict(good, stage=dict(window_s=8))):
            with self.assertRaises(ValueError):
                config.build('bad', bad)

    def test_code_id_follows_closure(self):
        """A builder's cache key changes with its source, the constants it reads and the helpers it calls, and not
        with unrelated code in the same module."""
        import importlib.util
        import linecache
        src = """C = {c}
OTHER = {o}


def helper(x):
    return x + C


def unrelated():
    return OTHER


def builder(x):
    return helper(x) * {k}
"""
        name = 'tools.audio_analysis.lab._closure_probe'

        def ident(c, o, k):
            with tempfile.TemporaryDirectory() as d:
                f = Path(d) / '_closure_probe.py'
                f.write_text(src.format(c=c, o=o, k=k))
                linecache.clearcache()
                spec = importlib.util.spec_from_file_location(name, f)
                mod = importlib.util.module_from_spec(spec)
                sys.modules[name] = mod
                try:
                    spec.loader.exec_module(mod)
                    return cache.code_id(mod.builder)
                finally:
                    del sys.modules[name]
        base = ident(1, 1, 2)
        self.assertEqual(base, ident(1, 5, 2))
        self.assertNotEqual(base, ident(2, 1, 2))
        self.assertNotEqual(base, ident(1, 1, 3))

    def test_tree_weights_and_fit_match_kick_loop(self):
        """The trees part weighs rows as the kick fast loop's training_set and fits the same model: equal
        predictions on the same rows."""
        from sklearn.ensemble import HistGradientBoostingClassifier

        from tools.audio_analysis.eval.kick_goal_eval import training_set
        rng = np.random.default_rng(2)
        records, rows_ = {}, []
        for k in range(5):
            n = 400
            x = rng.normal(size=(n, 6))
            y = (x[:, 0] + rng.normal(size=n) > 1).astype(float)
            hard = (y == 0) & (rng.uniform(size=n) < .1)
            mask = rng.uniform(size=n) < .9
            only = k == 4
            records[f's{k}'] = dict(training_mask=mask, labels=y, hard_neg=hard, features=x, positives_only=only)
            m = mask & (y == 1) if only else mask
            rows_.append((x[m], y[m], hard[m], False))
        p = dict(max_iter=50, learning_rate=.05, max_leaf_nodes=15, min_samples_leaf=40, l2=1.0)
        mine = models.fit_trees(p, rows_, 7)
        g = type('G', (), dict(records=records))()
        x, y, w = training_set(g, list(records))
        theirs = HistGradientBoostingClassifier(max_iter=50, learning_rate=.05, max_leaf_nodes=15, min_samples_leaf=40,
                                                l2_regularization=1.0, random_state=7).fit(x, y, sample_weight=w * len(y))
        probe = rng.normal(size=(200, 6))
        np.testing.assert_array_equal(mine.predict_proba(probe), theirs.predict_proba(probe))
        # A share makes clip songs together weigh share of everything.
        ws = models.weights([r[1] for r in rows_], [r[2] for r in rows_], [False, False, False, True, True], .25)
        clip = ws[3].sum() + ws[4].sum()
        self.assertAlmostEqual(clip / (clip + sum(v.sum() for v in ws[:3])), .25)

    def test_mean_logit_is_kick_blend(self):
        from scipy.special import expit, logit
        rng = np.random.default_rng(3)
        a, b = rng.uniform(size=50), rng.uniform(size=50)
        a[0], b[1] = 0.0, 1.0
        want = expit((logit(np.clip(a, 1e-6, 1 - 1e-6)) + logit(np.clip(b, 1e-6, 1 - 1e-6))) / 2)
        np.testing.assert_allclose(models.combine('mean_logit', dict(net=a, trees=b)), want, rtol=0, atol=1e-15)

    def test_exam_rejects_generated(self):
        songs = load()
        gen = next(t for t, s in songs.items() if s.kind == 'generated')
        proj = next(t for t, s in songs.items() if s.kind == 'project')
        run.check_exam([proj], songs)
        with self.assertRaises(ValueError):
            run.check_exam([proj, gen], songs)
        with self.assertRaises(ValueError):
            run.check_exam([proj], songs, failed=[proj])

    def test_held_out_disjoint(self):
        songs = load()
        lab, _ = run.labels('snare')
        for name in ('snare_base', 'snare_generated', 'snare_release'):
            cfg = config.load(name)
            grps, trainings, untrained = models.plan(cfg, lab, songs)
            models.check_disjoint(trainings)
            self.assertEqual(sorted(t for g in grps for t in g), sorted(lab))
            outer = [n for n in trainings if len(n['without']) == 1]
            self.assertEqual(sorted(t for n in outer for t in n['held']), sorted(lab))
            pairs = [n for n in trainings if len(n['without']) == 2]
            self.assertEqual(len(pairs), len(grps) * (len(grps) - 1) // 2 if cfg.folds else 0)
            # Every song outside group k has exactly one prediction for k's cutoff, from a model without k or it.
            for k, g in enumerate(grps if cfg.folds else ()):
                keys = [t for n in pairs for t, pre in models.save_keys(grps, n).items() if pre == f'pair{k}']
                self.assertEqual(sorted(keys), sorted(t for j, h in enumerate(grps) if j != k for t in h))
        trainings[0]['trained'].append(trainings[0]['held'][0])
        with self.assertRaises(ValueError):
            models.check_disjoint(trainings)

    def test_compare_refuses_mismatch(self):
        a = dict(run_id='a', labels_sha256='x', exam=['s1', 's2'], recall_only=[])
        compare.check(a, dict(a, run_id='b'))
        for k, v in (('labels_sha256', 'y'), ('exam', ['s1']), ('recall_only', ['s1'])):
            with self.assertRaises(ValueError):
                compare.check(a, dict(a, run_id='b', **{k: v}))

    def test_scorer_parity(self):
        """The lab scorer on saves of the retired runners reproduces their results files exactly, each on the labels
        it was scored with (snare: rolls still in; bass: the old 30 ms window and fitted shift)."""
        import gzip
        for golden, cfg in (('legacy_snare_folds4_s0', 'snare_base'), ('legacy_bass_folds4_s0', 'bass_base')):
            g = GOLDEN.with_name(golden)
            with gzip.open(g.with_suffix('.labels.json.gz'), 'rt') as f:
                lab = json.load(f)
            got = json.loads(json.dumps(score.legacy(g.with_suffix('.npz'), cfg, lab=lab), default=float))
            want = json.loads(g.with_suffix('.json').read_text())['results']['net']['per_song']
            self.assertEqual(sorted(got), sorted(want), golden)
            for t in want:
                self.assertEqual(got[t], want[t], f'{golden} {t}')

    def test_input_parity(self):
        """The lab hands the net trainer what the retired snare runner did: song order, groups, untrained songs, and
        byte-identical Song inputs (one song per audio source kind)."""
        from tools.audio_analysis.eval.detector_eval import HOP, rows
        from tools.audio_analysis.eval.detector_net import groups
        from tools.audio_analysis.eval.detector_songs import audio, rate, spec
        from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
        from tools.audio_analysis.eval.kick_goal_nn import Song as NetSong
        from tools.audio_analysis.eval.snare_cands import candidates
        songs = load()
        lab, _ = run.labels('snare')
        legacy = json.loads((GOAL / 'snare_labels.json').read_text())
        gen = json.loads((GOAL / 'snare_labels_generated.json').read_text())
        legacy.update(gen)
        self.assertEqual(list(lab), list(legacy))
        grps, trainings, untrained = models.plan(BASE, lab, songs)
        self.assertEqual(untrained, set(gen))
        self.assertEqual(grps, groups(list(legacy), 4))
        self.assertEqual([n['held'] for n in trainings[:4]], grps)
        self.assertEqual(models.plan(config.load('snare_generated'), lab, songs)[2], set())
        g = Goal(mode='v3')
        picks = [next(t for t in lab if songs[t].source['how'] == how) for how in ('goal', 'wip', 'section')]
        cpu = __import__('torch').device('cpu')
        mine = run.build(BASE, {t: lab[t] for t in picks}, songs, cpu, log=lambda m: None)
        for t in picks:
            sr = rate(g, t)
            cand, avail = candidates(audio(g, t), sr, 4.5)
            onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
            mask, y, hard = rows(legacy[t], onset)
            old = NetSong(spec(g, t), onset, emit + .04, mask, y, t, hard)
            new = mine[t]
            for k in ('spec', 'base'):
                self.assertTrue(bool((getattr(old, k) == getattr(new, k)).all()), (t, k))
            for k in ('end', 'pre', 'on', 'mask', 'y', 'hard'):
                np.testing.assert_array_equal(getattr(old, k), getattr(new, k), err_msg=f'{t} {k}')

    def test_input_parity_note_family(self):
        """Bass through the lab gets the retired family runner's inputs: its candidates in the bass band, rows and
        noisiness channel, byte for byte."""
        from tools.audio_analysis.eval import bass_labels
        from tools.audio_analysis.eval.detector_cands import HOP
        from tools.audio_analysis.eval.detector_channels import flat_cache
        from tools.audio_analysis.eval.detector_eval import rows
        from tools.audio_analysis.eval.detector_net import family_candidates
        from tools.audio_analysis.eval.detector_songs import audio, rate, spec
        from tools.audio_analysis.eval.kick_goal_eval import Goal
        from tools.audio_analysis.eval.kick_goal_nn import Song as NetSong
        cfg = config.load('bass_base')
        songs = load()
        lab, _ = run.labels('bass')
        g = Goal(mode='v3')
        picks = [next(t for t in lab if songs[t].source['how'] == how) for how in ('goal', 'wip')]
        cpu = __import__('torch').device('cpu')
        mine = run.build(cfg, {t: lab[t] for t in picks}, songs, cpu, log=lambda m: None)
        cands = family_candidates(cfg.candidates, bass_labels.FAMILY)
        for t in picks:
            sr, sp = rate(g, t), spec(g, t)
            cand, avail = cands(audio(g, t), sr, sp)
            onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
            mask, y, hard = rows(lab[t], onset)
            old = NetSong(sp, onset, emit + .04, mask, y, t, hard, (flat_cache(g, t),))
            new = mine[t]
            for k in ('spec', 'base'):
                self.assertTrue(bool((getattr(old, k) == getattr(new, k)).all()), (t, k))
            self.assertTrue(bool((old.extra[0] == new.extra[0]).all()), (t, 'flat'))
            for k in ('end', 'pre', 'on', 'mask', 'y', 'hard'):
                np.testing.assert_array_equal(getattr(old, k), getattr(new, k), err_msg=f'{t} {k}')


if __name__ == '__main__':
    unittest.main()
