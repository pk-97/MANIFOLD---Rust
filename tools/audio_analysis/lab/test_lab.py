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


_PATH = None


def _reads_path():
    return _PATH


class LabTests(unittest.TestCase):
    def test_code_id_same_in_every_worktree(self):
        """A builder reading a path inside the checkout gets one key wherever the checkout lives, so trained models
        are shared across worktrees."""
        global _PATH
        old = cache.REPO
        keys = []
        try:
            for root in (old, Path('/elsewhere/slot-9')):
                cache.REPO = root
                _PATH = root / 'tools' / 'x.json'
                keys.append(cache.code_id(_reads_path))
        finally:
            cache.REPO = old
        self.assertEqual(keys[0], keys[1])

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
        songs = {t: type('S', (), dict(kind='project', gain_db=None, parent=None, probe=False))() for t in lab}
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
        self.assertEqual(cfg.models, dict(net=dict(past_ms=190, ahead_ms=40, channels=['flat'], epochs=12,
                                                   swa_epochs=0, batch=256, lr=.002, precision='float32')))
        self.assertEqual((cfg.combine, cfg.stage, cfg.ahead_s), ('net', None, .04))
        good = config.migrate(old)
        # A schema 2 net written before the training options reads them as today's training.
        before = dict(good, models=dict(net={k: good['models']['net'][k] for k in ('past_ms', 'ahead_ms', 'channels', 'epochs')}))
        self.assertEqual(config.build('before', before).models, cfg.models)
        from tools.audio_analysis.eval import kick_goal_nn as nn
        self.assertEqual((nn.BATCH, nn.LR), (config.NET_DEFAULTS['batch'], config.NET_DEFAULTS['lr']))
        for f in config.EXPERIMENTS.glob('*.json'):
            raw = json.loads(f.read_text())['models']
            self.assertTrue('net' not in raw or set(raw['net']) == config.NET_KEYS, f.name)
        trees = dict(measures=['tail3'], max_iter=10, learning_rate=.1, max_leaf_nodes=7, min_samples_leaf=5, l2=1.0)
        config.build('both', dict(good, models=dict(good['models'], trees=trees), combine='mean_logit'))
        for bad in (dict(good, extra=1), dict(good, combine='trees'),
                    dict(good, models=dict(good['models'], trees=trees)),
                    dict(good, models=dict(trees=dict(trees, measures=['nope'])), combine='trees'),
                    dict(good, models=dict(net=dict(good['models']['net'], channels=['nope']))),
                    dict(good, models=dict(net=dict(good['models']['net'], swa_epochs=13))),
                    dict(good, models=dict(net=dict(good['models']['net'], precision='half'))),
                    dict(good, stage=dict(window_s=8))):
            with self.assertRaises(ValueError):
                config.build('bad', bad)

    def test_code_id_follows_closure(self):
        """A builder's cache key changes with its source, the constants it reads and the helpers it calls, and not
        with unrelated code in the same module."""
        import importlib.util
        import linecache
        src = """import functools
C = {c}
OTHER = {o}
TABLE = {{'a': {{1, 2}}, 'b': [3]}}


@functools.lru_cache(maxsize=None)
def cached(x):
    return x + {w}


def helper(x):
    return x + C + len(TABLE['a']) + cached(x)


def unrelated():
    return OTHER


def builder(x):
    return helper(x) * {k}
"""
        name = 'tools.audio_analysis.lab._closure_probe'

        def ident(c, o, k, table=None, w=1):
            with tempfile.TemporaryDirectory() as d:
                f = Path(d) / '_closure_probe.py'
                text = src.format(c=c, o=o, k=k, w=w)
                f.write_text(text if table is None else text.replace("TABLE = {'a': {1, 2}, 'b': [3]}", table))
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
        self.assertEqual(base, ident(1, 1, 2, table="TABLE = {'b': [3], 'a': {2, 1}}"))
        self.assertNotEqual(base, ident(1, 1, 2, table="TABLE = {'a': {1, 2, 4}, 'b': [3]}"))
        self.assertNotEqual(base, ident(1, 1, 2, w=2))

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

    def test_kick_labels_fresh(self):
        """kick_labels.json is what the builder makes now, and the builder's gate held: on every song, rows() over the
        labels before rolls equals the kick goal record's training mask and y."""
        import json as _json

        from tools.audio_analysis.eval import kick_labels
        from tools.audio_analysis.eval.kick_goal_eval import GOAL
        built = _json.loads(_json.dumps(kick_labels.build(write=False)))
        self.assertEqual(built, _json.loads((GOAL / 'kick_labels.json').read_text()))

    def test_kick_parts_on_audio_match_training(self):
        """The release reference's kick finder and measures (components on the song's audio, as release.py runs them on
        a clip) equal the cached ones every run trains on, byte for byte, and the finder equals the kick goal records'
        candidates (one 44.1 kHz song, two 48 kHz)."""
        from tools.audio_analysis.eval.kick_goal_eval import Goal
        from tools.audio_analysis.lab import components
        from tools.audio_analysis.lab.registry import mix
        songs = load()
        g = Goal(mode='v3')
        for t in ('bad_guy_128bpm', 'apricots_128bpm', 'tears_140bpm'):
            song = songs[t]
            x = mix(song)
            ref = components.audio_candidates('kick', 'kick', x, song.sample_rate)
            c = components.candidates(song, 'kick', 'kick')
            np.testing.assert_array_equal(c.cand, ref.cand, err_msg=t)
            np.testing.assert_array_equal(c.avail, ref.avail, err_msg=t)
            self.assertEqual(c.hop, ref.hop, t)
            np.testing.assert_array_equal(c.cand, g.records[t]['candidates'], err_msg=t)
            np.testing.assert_array_equal(c.avail, g.records[t]['available'], err_msg=t)
            for m in ('f15', 'tail3', 'prof32', 'low16'):
                np.testing.assert_array_equal(components.measure(song, 'kick', 'kick', m),
                                              components.MEASURES[m](x, song.sample_rate, ref), err_msg=f'{t} {m}')

    def test_layers_shared_across_configs(self):
        """A model trained for one config is reused by another that trains the same model, and only by it: kick_net
        and kick_blend share nets, kick_trees and kick_blend share trees, and a change to the trees' settings or
        the sampling makes new keys."""
        def keys(cfg):
            meta = run.setup(cfg)[2]
            return {m: run.model_key(cfg, m, meta) for m in cfg.models}
        net, trees, blend = (keys(config.load(c)) for c in ('kick_net', 'kick_trees', 'kick_blend'))
        self.assertEqual(blend, dict(net=net['net'], trees=trees['trees']))
        d = config.load('kick_blend')
        other = config.Experiment(**{**d.__dict__, 'models': dict(d.models, trees=dict(d.models['trees'], max_iter=50))})
        k = keys(other)
        self.assertEqual(k['net'], net['net'])
        self.assertNotEqual(k['trees'], trees['trees'])
        folds = config.Experiment(**{**d.__dict__, 'folds': 8 if d.folds != 8 else 4})
        self.assertNotEqual(keys(folds)['net'], net['net'])

    def test_changed_source_refuses_to_run(self):
        """A source file whose bytes moved since the song list was built stops the run before anything trains."""
        from tools.audio_analysis.lab import registry
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / 'a.npy'
            np.save(f, np.zeros(10, np.float32))
            s = song('x', f)
            old = registry.HASHES
            registry.HASHES = Path(d) / 'hashes.json'
            try:
                registry.check_sources([s])
                np.save(f, np.ones(10, np.float32))
                with self.assertRaises(SystemExit):
                    registry.check_sources([s])
            finally:
                registry.HASHES = old

    def test_model_key_follows_what_trains(self):
        """A model's key moves with the labels of songs it trains on and with any song's audio, never with the labels
        of a song it only predicts."""
        cfg = config.load('kick_trees')
        trainings = [dict(without=[0], held=['a'], trained=['b', 'c'], clips=[]),
                     dict(without=[1], held=['b'], trained=['a', 'c'], clips=[])]
        meta = dict(parts=dict(finder='f', measures='m'), trainings=trainings, label_ids=dict(a='1', b='2', c='3', z='9'),
                    content=dict(a='A', b='B', c='C'))
        base = run.model_key(cfg, 'trees', meta)
        self.assertEqual(base, run.model_key(cfg, 'trees', dict(meta, label_ids=dict(meta['label_ids'], z='8'))))
        self.assertNotEqual(base, run.model_key(cfg, 'trees', dict(meta, label_ids=dict(meta['label_ids'], c='4'))))
        self.assertNotEqual(base, run.model_key(cfg, 'trees', dict(meta, content=dict(meta['content'], a='A2'))))

    def test_inside_and_covered(self):
        """inside() equals the plain loop over closed spans; covered() counts overlaps once."""
        from tools.audio_analysis.eval.detector_eval import covered, inside
        rng = np.random.default_rng(4)
        for _ in range(500):
            spans = [(float(a), float(a + rng.uniform(-.2, 2))) for a in rng.uniform(0, 10, rng.integers(0, 20))]
            t = np.concatenate([rng.uniform(-1, 12, 40), [x for sp in spans for x in sp]])
            want = np.zeros(len(t), bool)
            for a, b in spans:
                want |= (t >= a) & (t <= b)
            np.testing.assert_array_equal(inside(t, spans), want)
            grid = np.linspace(0, 10, 200001)
            self.assertAlmostEqual(covered(spans, 0.0, 10.0), inside(grid, spans).mean() * 10, delta=.002 * (1 + len(spans)))

    def test_false_per_minute_skips_recall_only(self):
        from tools.audio_analysis.eval.detector_compare import timing
        side = [dict(a=dict(minutes=2.0, extra=4, late_ms=[]), r=dict(minutes=8.0, extra=0, late_ms=[]))]
        self.assertIn('2.00 false per minute', timing(side, ['a', 'r'], recall_only=('r',)))

    def test_score_refuses_mismatched_save(self):
        """Saved probabilities that do not line up with the cached candidates are an error, never a silent pairing."""
        import gzip
        g = GOLDEN.with_name('legacy_snare_folds4_s0')
        with gzip.open(g.with_suffix('.labels.json.gz'), 'rt') as f:
            lab = json.load(f)
        with np.load(g.with_suffix('.npz')) as z:
            t = z.files[0]
            held = {t: z[t][:-1]}
        with self.assertRaises(ValueError):
            score.score(BASE, lab, load(), held)

    def test_generated_audit_catches_mislabels(self):
        """The generated audit fails a label on no note and a note left unlabelled outside the unscored spans, and
        passes a note inside them."""
        notes = np.array([1.0, 2.0, 3.0, 4.0])
        s = dict(positives=[1.0, 2.0, 3.0], unscored=[[3.9, 4.1]], section=[0.0, 10.0])
        self.assertEqual(audit.generated_check(s, notes)['why'], [])
        self.assertTrue(audit.generated_check(dict(s, positives=[1.0, 2.0, 3.0, 5.0]), notes)['why'])
        self.assertTrue(audit.generated_check(dict(s, unscored=[]), notes)['why'])

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
        for name in ('snare_base', 'snare_generated', 'snare_v1'):
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

    def test_gain_variants(self):
        """Gain copies train with their song's group and are never held out; null changes nothing; a copy's audio is
        the song's, scaled, under its own content id; probes are held out with their song and never trained."""
        from tools.audio_analysis.lab.registry import gain_copies, gain_copy, gain_gains, mix
        base = config.load('snare_v2_mid')
        raw = json.loads((config.EXPERIMENTS / 'snare_v2_mid.json').read_text())
        for sampling in (dict(raw['sampling'], gain_variants=None), dict(raw['sampling'], gain_probes=None)):
            self.assertEqual(config.build('x', dict(raw, sampling=sampling)).key(), base.key())
        gv = config.load('snare_v2_gain')
        self.assertEqual(gv.gain_variants, dict(count=3, db=[-12, 6]))
        self.assertEqual({k: v for k, v in gv.sampling.items() if k != 'gain_variants'}, base.sampling)
        with self.assertRaises(ValueError):
            config.build('x', dict(raw, sampling=dict(raw['sampling'], gain_variants=dict(count=0, db=[-1, 1]))))
        songs = load()
        lab, _ = run.labels('snare')
        exam = [t for t in lab if songs[t].kind == 'project']
        plain = models.plan(base, lab, songs)
        for t in exam:
            for c in gain_copies(songs[t], 3, -12, 6) + [gain_copy(songs[t], -6, probe=True)]:
                songs[c.id], lab[c.id] = c, lab[t]
        grps, trainings, untrained = models.plan(base, lab, songs)
        self.assertEqual((grps, untrained), plain[:1] + plain[2:])
        copy = {t: [c for c in lab if songs[c].parent == t and not songs[c].probe] for t in exam}
        probe = {t: [c for c in lab if songs[c].parent == t and songs[c].probe] for t in exam}
        self.assertTrue(all(len(v) == 3 for v in copy.values()) and all(len(v) == 1 for v in probe.values()))
        models.check_disjoint(trainings, {c: songs[c].parent for c in lab if songs[c].parent})
        for n, old in zip(trainings, plain[1]):
            held = {t for t in old['held']}
            self.assertEqual([t for t in n['held'] if t in held], old['held'])
            if len(n['without']) == 1:
                self.assertEqual(sorted(set(n['held']) - held), sorted(c for t in held for c in probe.get(t, [])))
            else:
                self.assertEqual(set(n['held']), held)
            # Each held song's copies stay out of the training; every other song's copies are in it, probes never.
            self.assertFalse(any(c in n['trained'] for t in held for c in copy.get(t, []) + probe.get(t, [])))
            self.assertTrue(all(c in n['trained'] for t in exam if t not in held for c in copy[t]))
            self.assertFalse(any(songs[u].probe for u in n['trained']))
            self.assertEqual([u for u in n['trained'] if songs[u].gain_db is None], old['trained'])
        t = exam[0]
        self.assertEqual(gain_gains(t, 3, -12, 6), gain_gains(t, 3, -12, 6))
        self.assertNotEqual(gain_gains(t, 3, -12, 6), gain_gains(exam[1], 3, -12, 6))
        with tempfile.TemporaryDirectory() as d:
            a = Path(d) / 'a.npy'
            np.save(a, np.linspace(-.5, .5, 100))
            s = song('s', a)
            c = gain_copy(s, -6.0)
            np.testing.assert_allclose(mix(c), mix(s) * 10 ** (-6 / 20))
            self.assertEqual(mix(s).tolist(), np.load(a).tolist())
            self.assertNotEqual(c.content_id, s.content_id)
            self.assertNotEqual(c.content_id, gain_copy(s, -5.0).content_id)
        self.assertEqual(cache.code_id(mix), cache.code_id(mix.__wrapped__))

    def test_row_weight_options(self):
        """sampling.hard_negatives and sampling.coincident_weight: null reads as absent (the key and run ids stay as
        before), bad values are refused, and each rides the per-model key through the sampling dict."""
        base = config.load('snare_v2_mid')
        raw = json.loads((config.EXPERIMENTS / 'snare_v2_mid.json').read_text())

        def with_sampling(**kw):
            return dict(raw, sampling=dict(raw['sampling'], **kw))
        for off in (dict(hard_negatives=None), dict(coincident_weight=None)):
            self.assertEqual(config.build('x', with_sampling(**off)).key(), base.key())
        hard, co = config.load('snare_v2_hardneg'), config.load('snare_v2_coincident')
        self.assertEqual(hard.hard_negatives, dict(by='onset', power=1.0))
        self.assertEqual(co.coincident_weight, 3.0)
        self.assertEqual({k: v for k, v in hard.sampling.items() if k != 'hard_negatives'}, base.sampling)
        self.assertEqual({k: v for k, v in co.sampling.items() if k != 'coincident_weight'}, base.sampling)
        for f in ('snare_v2_hardneg', 'snare_v2_coincident'):   # the shipped recipe plus the one option, nothing else
            a, b = json.loads((config.EXPERIMENTS / f'{f}.json').read_text()), dict(raw)
            self.assertEqual({k: v for k, v in a.items() if k != 'sampling'}, {k: v for k, v in b.items() if k != 'sampling'})
        for bad in (dict(hard_negatives=dict(by='onset')), dict(hard_negatives=dict(by='level', power=1.0)),
                    dict(hard_negatives=dict(by='onset', power=0)), dict(hard_negatives=dict(by='onset', power=True)),
                    dict(hard_negatives=dict(by='onset', power=1.0, extra=1)), dict(hard_negatives=2.0),
                    dict(coincident_weight=0), dict(coincident_weight=-1.0), dict(coincident_weight=True),
                    dict(coincident_weight='3')):
            with self.assertRaises(ValueError, msg=str(bad)):
                config.build('bad', with_sampling(**bad))
        trees_only = dict(raw, models=dict(trees=raw['models']['trees']), combine='trees', stage=None)
        for opt in (dict(hard_negatives=dict(by='onset', power=1.0)), dict(coincident_weight=2.0)):
            with self.assertRaises(ValueError, msg='a trees-only config would ignore it'):
                config.build('bad', dict(trees_only, sampling=dict(raw['sampling'], **opt)))
        with self.assertRaises(ValueError):
            config.build('bad', dict(with_sampling(coincident_weight=2.0), family='bass'))
        self.assertEqual(config.COINCIDENT_FAMILIES, tuple(score.PARTNER))
        # The per-model key moves with each option, and with the partner labels the draws read.
        trainings = [dict(without=[0], held=['a'], trained=['b', 'c'], clips=[]),
                     dict(without=[1], held=['b'], trained=['a', 'c'], clips=[])]
        meta = dict(parts=dict(finder='f', frames='fm', measures='m'), trainings=trainings,
                    label_ids=dict(a='1', b='2', c='3'), content=dict(a='A', b='B', c='C'))
        keys = {c.name: run.model_key(c, 'net', meta) for c in (base, hard, co)}
        self.assertEqual(len(set(keys.values())), 3)
        self.assertNotEqual(run.model_key(co, 'net', meta),
                            run.model_key(co, 'net', dict(meta, label_ids=dict(meta['label_ids'], b='9'))))
        self.assertNotEqual(keys['snare_v2_hardneg'], run.model_key(hard, 'net', dict(meta, parts=dict(meta['parts'], onset='o'))))

    def test_row_weights_follow_the_spec(self):
        """hard_negatives: a candidate's weight is its onset strength (the largest early rise of the cached profile, at
        least one dB) to the power. coincident_weight: W on positive rows whose nearest label sits within 20 ms of a
        partner-family label, 1 on every other row, negatives included."""
        pairs = 16
        profile = np.zeros((4, 2 * pairs))
        profile[0, 3], profile[1, 15], profile[2, pairs + 5], profile[3, 0] = 6.0, 12.0, 40.0, .2
        strength = models.onset_strength(profile)
        np.testing.assert_array_equal(strength, [6.0, 12.0, models.HARD_FLOOR_DB, models.HARD_FLOOR_DB])
        np.testing.assert_allclose(models.hard_negative_weights(strength, 1.0), [6.0, 12.0, 1.0, 1.0])
        np.testing.assert_allclose(models.hard_negative_weights(strength, 2.0), [36.0, 144.0, 1.0, 1.0])
        # Snare labels at 1, 2, 3; kicks at 1.010 (together), 2.025 (not), 3.020 (exactly 20 ms: together).
        onset = np.array([.5, 1.002, 1.5, 2.001, 2.9, 3.001, 4.0])
        y = np.array([0., 1., 0., 1., 0., 1., 0.])
        w = models.coincident_row_weights(onset, y, [3.0, 1.0, 2.0], [1.010, 2.025, 3.020], 3.0)
        np.testing.assert_array_equal(w, [1, 3, 1, 1, 1, 3, 1])
        # The partner mirror: a kick row beside a snare label.
        w = models.coincident_row_weights(np.array([1.011, 2.024]), np.array([1., 1.]), [1.010, 2.025], [1.0, 2.0], 2.0)
        np.testing.assert_array_equal(w, [2, 1])
        np.testing.assert_array_equal(models.coincident_row_weights(onset, y, [], [1.0], 3.0), np.ones(len(onset)))
        np.testing.assert_array_equal(models.coincident_row_weights(onset, y, [1.0], [], 3.0), np.ones(len(onset)))
        # run.row_weights wires them: a gain copy reads its parent's partner labels, an unlabelled song draws evenly,
        # and a config without the options hands the net nothing.
        import types

        from tools.audio_analysis.lab import components
        mine, copy = dict(positives=[3.0, 1.0, 2.0]), types.SimpleNamespace(id='a_g-6', parent='a')
        partner = dict(a=dict(positives=[1.010, 2.025, 3.020]))
        neg_w, pos_w = run.row_weights(config.load('snare_v2_coincident'), copy, mine, onset, y, partner)
        self.assertIsNone(neg_w)
        np.testing.assert_array_equal(pos_w, [1, 3, 1, 1, 1, 3, 1])
        other = types.SimpleNamespace(id='b', parent=None)
        self.assertEqual(run.row_weights(config.load('snare_v2_coincident'), other, mine, onset, y, partner), (None, None))
        self.assertEqual(run.row_weights(config.load('snare_v2_mid'), copy, mine, onset, y, None), (None, None))
        real = components.measure
        components.measure = lambda song, rule, family, name: profile
        try:
            neg_w, pos_w = run.row_weights(config.load('snare_v2_hardneg'), copy, mine, onset[:4], y[:4], None)
        finally:
            components.measure = real
        np.testing.assert_allclose(neg_w, [6.0, 12.0, 1.0, 1.0])
        self.assertIsNone(pos_w)

    def test_compare_refuses_mismatch(self):
        a = dict(run_id='a', labels_sha256='x', exam=['s1', 's2'], recall_only=[])
        compare.check(a, dict(a, run_id='b'))
        for k, v in (('labels_sha256', 'y'), ('exam', ['s1']), ('recall_only', ['s1'])):
            with self.assertRaises(ValueError):
                compare.check(a, dict(a, run_id='b', **{k: v}))

    def test_child_refuses_cpu_unless_asked(self):
        """A net child on the CPU exits with the reason unless --cpu; the GPU, and --cpu on the CPU, pass; run() hands
        --cpu on to the child. train() called directly (the parity tests) never meets the guard."""
        import subprocess
        from unittest import mock

        import torch
        cpu, mps = torch.device('cpu'), torch.device('mps')
        with self.assertRaises(SystemExit) as e:
            run.check_device(cpu)
        self.assertIn('refusing to train a net on the CPU', str(e.exception))
        self.assertIn('--cpu', str(e.exception))
        with mock.patch.dict(os.environ, {'KICK_GOAL_NN_DEVICE': 'cpu'}):
            with self.assertRaises(SystemExit) as e:
                run.check_device(cpu)
            self.assertIn('KICK_GOAL_NN_DEVICE=cpu', str(e.exception))
        run.check_device(cpu, cpu=True)
        run.check_device(mps)
        seen = []

        def fake(cmd, **kw):
            seen.append(cmd)
            return subprocess.CompletedProcess(cmd, 0)
        for flag in (False, True):
            seen.clear()
            with mock.patch.object(run, 'run_id_of', return_value='kick-none'), \
                    mock.patch.object(run.subprocess, 'run', fake), mock.patch.object(run, 'RUNS', Path(tempfile.mkdtemp())):
                run.run('kick_v2_final', [7], cpu=flag)
            self.assertEqual(['--child' in c and '--cpu' in c for c in seen], [flag])

    def test_old_exam_view(self):
        """--exam-of: the A/B on the songs of an earlier run's exam, beside the untouched full-exam numbers."""
        from unittest import mock

        from tools.audio_analysis.eval.detector_compare import pooled

        def side(hits):
            ps = {t: dict(matched=h, labels=10, extra=e, minutes=1.0, late_ms=[]) for t, (h, e) in hits.items()}
            return dict(exam=list(hits), recall_only=[], results=dict(net=dict(per_song=ps)))
        a = [side(dict(s1=(5, 1), s2=(6, 1), s3=(9, 0), s4=(9, 0)))]
        b = [side(dict(s1=(7, 1), s2=(8, 1), s3=(4, 4), s4=(4, 4)))]
        before = json.dumps([a, b], sort_keys=True)
        with tempfile.TemporaryDirectory() as d, mock.patch.object(score, 'RUNS', Path(d)):
            (Path(d) / 'old-run').mkdir()
            (Path(d) / 'old-run' / 'meta.json').write_text(json.dumps(dict(exam=['s1', 's2', 'gone'])))
            kept, head = score.old_exam('old-run', a[0]['exam'])
            self.assertEqual(kept, ['s1', 's2'])
            self.assertIn('2 of its 3 songs', head)
            self.assertIn('out of 4 in this exam', head)
            self.assertIn("not in this exam: ['gone']", head)
            lines = compare.old_exam_lines([a, b], 'old-run', 'snare')
            self.assertTrue(all(ln.startswith('old exam | ') for ln in lines))
            self.assertIn('2 songs', '\n'.join(lines))
            self.assertIn('| 2 songs', lines[2])   # A: F1 line counts the subset only
            sub = pooled({t: a[0]['results']['net']['per_song'][t] for t in kept}, kept, ())
            self.assertIn(f'{sub[2]:.4f}', lines[2])
            self.assertNotIn(f'{pooled(a[0]["results"]["net"]["per_song"], a[0]["exam"], ())[2]:.4f}', lines[2])
            rows = score.old_exam_lines({0: dict(a[0], run_id='x')}, 'old-run')
            self.assertIn('full exam (4 songs)', rows[1])
            self.assertIn('old exam (2 songs)', rows[1])
            with self.assertRaises(SystemExit):
                score.old_exam('no-such-run', [])
        self.assertEqual(before, json.dumps([a, b], sort_keys=True))

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
            from tools.audio_analysis.eval.detector_eval import covered
            for t in want:
                # Scored minutes count overlapping unscored spans once since the P6 review; all else is unchanged
                # (the gap recall keys are new).
                self.assertEqual({k: got[t][k] for k in want[t] if k != 'minutes'},
                                 {k: v for k, v in want[t].items() if k != 'minutes'}, f'{golden} {t}')
                self.assertAlmostEqual(got[t]['minutes'],
                                       max(0.0, lab[t]['duration_s'] - covered(lab[t]['unscored'], 0.0, lab[t]['duration_s'])) / 60,
                                       places=12)

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
            self.assertEqual((new.neg_w, new.pos_w), (None, None), 'a config without the options draws as before')

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

    def test_slices_on_device(self):
        """Net slices cut from the song's tensors equal the old CPU cut (numpy frame indices) bit for bit, with and
        without the training jitter, from array or tensor indices, with an extra channel."""
        import torch
        from tools.audio_analysis.eval import kick_goal_nn as nn
        from tools.audio_analysis.eval.kick_goal_nn import Song as NetSong

        def old_slices(song, idx, shift=None):
            end = song.end[idx] + (shift if shift is not None else 0)
            frames = torch.from_numpy(end[:, None] - np.arange(nn.SLICE)[::-1][None, :].copy())
            x = song.spec[frames]
            shape = x - x.amax(dim=(1, 2), keepdim=True)
            new = x - song.base[torch.from_numpy(np.asarray(idx))][:, None, :]
            chans = [shape, new] + [e[frames] for e in song.extra]
            return torch.stack(chans, 1).transpose(2, 3) / 20.0

        rng = np.random.default_rng(3)
        n = 4000
        spec = rng.normal(-40, 20, (n, nn.BANDS)).astype(np.float32)
        onset = np.sort(rng.uniform(0, n * nn.FRAME_S, 300))
        song = NetSong(spec, onset, onset + .01, np.ones(300, bool), np.zeros(300), 't',
                       extra=(rng.normal(0, 1, (n, nn.BANDS)).astype(np.float32),))
        for _ in range(5):
            idx = rng.choice(300, 500)
            shift = rng.integers(-nn.JITTER, nn.JITTER + 1, 500)
            for got, want in ((song.slices(idx), old_slices(song, idx)),
                              (song.slices(idx, shift), old_slices(song, idx, shift)),
                              (song.slices(torch.from_numpy(idx), torch.from_numpy(shift)), old_slices(song, idx, shift))):
                self.assertEqual(got.dtype, want.dtype)
                self.assertTrue(torch.equal(got, want))

    def test_stage_config(self):
        """A stage config is checked: it needs song groups, known measures and a [measure, column] level or none."""
        d = json.loads((config.EXPERIMENTS / 'kick_gen50_staged.json').read_text())
        config.build('ok', d)
        st = d['stage']
        for bad in (dict(d, folds=0), dict(d, stage=dict(st, shape=['nope'])), dict(d, stage=dict(st, level=['f15'])),
                    dict(d, stage=dict(st, windows_s=[])), dict(d, stage=dict(st, extra=1))):
            with self.assertRaises(ValueError):
                config.build('bad', bad)

    def test_stage_inputs_match_release(self):
        """The kick stage's inputs are the shipped kick's (self_features over prof32 and low16, level f15 column 0, 8 s;
        the v1 kick stage shape), from the lab's candidates and measures; without a level the level column is left
        out."""
        from tools.audio_analysis.eval.kick_goal_eval import Goal
        from tools.audio_analysis.eval.kick_goal_selfsim import self_features
        from tools.audio_analysis.lab import components, stage
        cfg = config.load('kick_gen50_staged')
        lab = run.labels('kick')[0]
        g = Goal(mode='v3')
        t = 'bad_guy_128bpm'
        song = load()[t]
        part = stage.song_parts(cfg, song, lab[t], build=False)
        np.testing.assert_array_equal(part[2], g.records[t]['emit_s'])
        p = np.random.default_rng(0).uniform(0, 1, len(part[2]))
        m = {k: components.measure(song, 'kick', 'kick', k, build=False) for k in ('f15', 'prof32', 'low16')}
        want = self_features(p, np.hstack([m['prof32'], m['low16']]), m['f15'][:, 0], part[2], 8.0)
        np.testing.assert_array_equal(stage.inputs(cfg.stage, part, p), want)
        no_level = stage.inputs(dict(cfg.stage, level=None, windows_s=[8.0, 4.0]), part, p)
        self.assertEqual(no_level.shape[1], 1 + 3 + 3)
        np.testing.assert_array_equal(no_level[:, :4], want[:, [0, 1, 2, 4]])

    def test_stage_never_sees_judged_song(self):
        """No staged prediction comes from a stage that learnt on the song, and the song's own inputs come from models
        that never trained on it: outer stages judge on the outer models, pair stages on the pair models. The outer
        stage also learns only from models that never saw the group it judges."""
        from unittest import mock

        from tools.audio_analysis.lab import stage
        cfg = config.build('x', json.loads((config.EXPERIMENTS / 'snare_gen50_staged.json').read_text()))
        cfg = config.Experiment(**{**cfg.__dict__, 'train': ('project',), 'sampling': dict(per_song='median')})
        lab = {t: {} for t in 'abcdefgh'}
        songs = {t: type('S', (), dict(kind='project', gain_db=None, parent=None, probe=False))() for t in lab}
        grps, trainings, _ = models.plan(cfg, lab, songs)
        code = {t: i for i, t in enumerate(lab)}
        name = {i: t for t, i in code.items()}
        n = 6
        parts = {t: (np.full((n, 1), code[t]), None, np.arange(n) * .5,
                     (np.ones(n, bool), np.arange(n) % 2.0, np.zeros(n, bool))) for t in lab}
        final = [{t: np.full(n, float(i)) for t in tr['held']} for i, tr in enumerate(trainings)]

        class Fit:
            def __init__(self, rows):
                self.seen = {(int(x[0, 0]), name[int(x[0, 1])]) for x, *_ in rows}

            def predict_proba(self, x):
                i, t = int(x[0, 0]), name[int(x[0, 1])]
                assert t not in {u for _, u in self.seen}, t
                assert t not in trainings[i]['trained'], t
                if len(trainings[i]['without']) == 1:
                    assert all(t not in trainings[j]['trained'] for j, _ in self.seen), t
                return np.column_stack([1 - x[:, 0] / 100, x[:, 0] / 100])
        with mock.patch.object(stage, 'inputs', lambda st, part, p: np.column_stack([p, part[0][:, 0]])), \
                mock.patch.object(models, 'fit_trees', lambda st, rows, rs, share: Fit(rows)):
            out = stage.train(cfg, grps, trainings, final, parts, 0, log=lambda m: None)
        at = {tuple(sorted(tr['without'])): i for i, tr in enumerate(trainings)}
        for k, g in enumerate(grps):
            for t in g:
                self.assertEqual(out[f'staged__{t}'][0] * 100, at[(k,)])
            for j, h in enumerate(grps):
                if j != k:
                    for u in h:
                        self.assertAlmostEqual(out[f'pair{k}__staged__{u}'][0] * 100, at[tuple(sorted((k, j)))])

    def test_stage_gate(self):
        """With an empty memory the gated score is the detector's own; with gate or more evidence it is the stage's;
        in between the change in logit scales with the evidence."""
        from scipy.special import expit, logit

        from tools.audio_analysis.lab import stage
        st = dict(level=None, gate=1.0)
        p = np.array([.3, .3, .3, .8])
        x = np.zeros((4, 4))
        x[:, 0] = logit(p)
        x[:, 3] = np.log1p([0.0, 0.5, 2.0, 0.0])
        q = np.array([.05, .05, .05, .05])
        got = stage.gated(st, x, q)
        np.testing.assert_allclose(got[[0, 3]], p[[0, 3]])
        np.testing.assert_allclose(got[1], expit(logit(.3) + .5 * (logit(.05) - logit(.3))))
        np.testing.assert_allclose(got[2], .05)
        np.testing.assert_array_equal(stage.gated(dict(st, gate=0), x, q), q)
        # Against its own cutoffs: an empty memory keeps the detector's score, full evidence moves it as far past the
        # detector's cutoff as the stage's score sits past its own.
        cut = stage.gated(st, x, q, (.7, .95))
        np.testing.assert_allclose(cut[[0, 3]], p[[0, 3]])
        np.testing.assert_allclose(logit(cut[2]) - logit(.7), logit(.05) - logit(.95))

    def test_after_gap(self):
        """A positive opens a passage only after a fully known gap with no positive, roll hit or run event in it."""
        from tools.audio_analysis.eval.detector_eval import after_gap
        s = dict(positives=[.5, 3.0, 3.4, 6.0, 9.0], dropped=[5.0], unscored=[[0, .1]])
        self.assertEqual(list(after_gap(s)), [1, 4])
        self.assertEqual(list(after_gap(dict(s, unscored=[[0, .1], [1.5, 1.6]]))), [4])
        self.assertEqual(list(after_gap(dict(s, dense_pos=[8.0]))), [1])


    def test_template_features_match_kick(self):
        """Given fixed templates, the lab's template features are kick_goal_templates' on the shipped kick's patches
        (band_spec of the mix at the finder's hop, candidate to emission), and with the template set the shipped
        kick used for the song they equal its tmpl3 cache."""
        from tools.audio_analysis.eval import kick_goal_templates as kt
        from tools.audio_analysis.eval.kick_goal_eval import GOAL
        from tools.audio_analysis.lab import components, templates
        from tools.audio_analysis.lab.registry import mix
        t = 'bad_guy_128bpm'
        song = load()[t]
        c = components.candidates(song, 'kick', 'kick', build=False)
        p = templates.patches(song, 'kick', 'kick')
        np.testing.assert_array_equal(p, kt.patches(kt.band_spec(mix(song), song.sample_rate, c.hop), c.cand, c.avail))
        dev = np.load(GOAL / 'templates_from_dev.npy')
        np.testing.assert_array_equal(templates.features(p, dev), kt.template_features(p, dev))
        np.testing.assert_array_equal(templates.features(p, np.load(GOAL / 'templates_from_new.npy')),
                                      np.load(GOAL / f'tmpl_{t}.npy'))

    def test_templates_never_see_held_song(self):
        """Each training's templates come from the positive training rows of its own trained songs and clips only:
        never a held song, a negative or an unscored row."""
        from unittest import mock

        from tools.audio_analysis.lab import templates
        cfg = config.load('kick_gen50_gated_t8')
        cfg = config.Experiment(**{**cfg.__dict__, 'models': dict(cfg.models, trees=dict(cfg.models['trees'],
                                                                                          templates=dict(k=2)))})
        lab = {t: {} for t in 'abcdefghyz'}
        songs = {t: type('S', (), dict(kind='generated' if t in 'yz' else 'project', gain_db=None, parent=None, probe=False))() for t in lab}
        grps, trainings, _ = models.plan(cfg, lab, songs)
        self.assertTrue(any(n['clips'] for n in trainings))
        code = {t: i for i, t in enumerate(lab)}
        n = 6
        y = np.array([1, 1, 0, 0, 1, 1.0])
        mask = np.array([True, True, True, True, True, False])
        tmpl = {}
        for t, i in code.items():
            yt = y * 0 if t == 'z' else y  # a clip without hits gives no patches
            p = np.zeros((n, 2, 32))
            p[yt == 1, 0, i] = 1      # positives mark the song at band i
            p[yt == 0, 0, i + 16] = 1  # negatives at band i + 16
            p[~mask, :, :] = 0
            p[~mask, 1, i] = 1        # the unscored positive in frame 1
            tmpl[t] = (p, (mask, yt, np.zeros(n, bool)))
        trees = {t: (np.full((n, 1), float(code[t])), None) for t in lab}
        seen = []

        def kmeans(x, w, k, rs):
            marks = {(int(r.argmax()) // 32, int(r.argmax()) % 32) for r in x}
            seen.append(marks)
            return x[:k]

        class Fit:
            def predict_proba(self, x):
                return np.column_stack([1 - x[:, 0] / 100, x[:, 0] / 100])
        with mock.patch.object(templates, '_kmeans', kmeans), \
                mock.patch.object(models, 'fit_trees', lambda p, rows, rs, share: Fit()):
            out, _ = run.train_templated(cfg, trainings, 0, trees, tmpl, log=lambda m: None)
        self.assertEqual(len(seen), len(trainings))
        for marks, tr, got in zip(seen, trainings, out):
            self.assertEqual(marks, {(0, code[u]) for u in tr['trained'] + tr['clips'] if u != 'z'}, tr['without'])
            self.assertFalse({code[u] for u in tr['held']} & {b for _, b in marks}, tr['without'])
            self.assertEqual(set(got), set(tr['held']))

    def test_templates_config(self):
        """templates is null or {"k": K >= 2}; a trees config without it reads null; the trees' key covers it and the
        net's does not."""
        d = json.loads((config.EXPERIMENTS / 'kick_gen50_gated_t8.json').read_text())
        trees = d['models']['trees']
        for bad in ({'k': 1}, {'k': 8.0}, {'k': True}, {'k': '8'}, {'k': 8, 'cap': 100}, {'n': 8}, {}, 8, 'on'):
            with self.assertRaises(ValueError, msg=bad):
                config.build('bad', dict(d, models=dict(d['models'], trees=dict(trees, templates=bad))))
        old = dict(d, models=dict(d['models'], trees={k: v for k, v in trees.items() if k != 'templates'}))
        self.assertIsNone(config.build('old', old).models['trees']['templates'])

        def keys(cfg):
            meta = run.setup(cfg)[2]
            return {m: run.model_key(cfg, m, meta) for m in cfg.models}
        base, t8, t16 = (keys(config.load(c)) for c in ('kick_gen50_gated', 'kick_gen50_gated_t8', 'kick_gen50_gated_t16'))
        self.assertEqual(base['net'], t8['net'])
        self.assertEqual(len({base['trees'], t8['trees'], t16['trees']}), 3)


REPO = Path(__file__).resolve().parents[3]
RELEASE = Path(__file__).with_name('release.py')
SET_NAMES = ('kick', 'snare', 'cover_kick', 'cover_snare')


def run_release(*args):
    """release.py in its own process, free of this process's net env (it pins the record's)."""
    env = {k: v for k, v in os.environ.items() if not k.startswith('KICK_GOAL_NN_')}
    r = __import__('subprocess').run([str(RELEASE), *args], capture_output=True, text=True, env=env)
    if r.returncode:
        raise AssertionError(f'release.py {" ".join(args)} failed:\n{r.stdout}\n{r.stderr}')
    return r.stdout


def exact(test, got, want, what):
    """Max abs diff 0 and equal shapes: any nonzero difference fails."""
    got, want = np.asarray(got), np.asarray(want)
    test.assertEqual(got.shape, want.shape, what)
    if got.dtype.kind in 'fiu' and got.size:
        test.assertEqual(float(np.max(np.abs(got.astype(np.float64) - want.astype(np.float64)))), 0.0, what)
    np.testing.assert_array_equal(got, want, err_msg=what)


class ReleaseTests(unittest.TestCase):
    """lab/release.py and the model file contract v2 (docs/DETECTOR_RUNTIME_DESIGN.md P1)."""

    @classmethod
    def setUpClass(cls):
        from tools.audio_analysis.lab import release
        cls.ids = {f: release.asset_train_id(f) for f in ('kick', 'snare')}

    def test_shipped_sets_are_their_release(self):
        """A fresh export of each shipped release equals its committed set and asset byte for byte."""
        from tools.audio_analysis.lab import release
        with tempfile.TemporaryDirectory() as d:
            for family in ('kick', 'snare'):
                run_release('export', self.ids[family], '--set', family, '--ship', '--out', d)
                fresh = Path(d) / release.SETS / family
                committed = REPO / release.SETS / family
                self.assertEqual(sorted(p.name for p in fresh.iterdir()), sorted(p.name for p in committed.iterdir()))
                for p in fresh.iterdir():
                    self.assertEqual(p.read_bytes(), (committed / p.name).read_bytes(), f'{family} {p.name}')
                asset = f'{family}_model.mkick'
                self.assertEqual((Path(d) / release.ASSETS / asset).read_bytes(), (REPO / release.ASSETS / asset).read_bytes())

    def test_contract_round_trip(self):
        """A record's entries pass the checker, survive the container byte for byte and are the committed model; every
        committed set's model passes and its clips name its train_id."""
        from tools.audio_analysis import kick_container as kc
        from tools.audio_analysis.lab import contract, release
        rec = release.load(self.ids['kick'])
        e = contract.entries(rec)
        contract.check(e, release.parts())
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'model.mkick'
            kc.write(path, e)
            back = kc.read(path)
            self.assertEqual(list(back), list(e))
            for k, v in e.items():
                self.assertEqual(back[k].dtype, np.asarray(v).dtype, k)
                np.testing.assert_array_equal(back[k], v, err_msg=k)
            contract.check(back, release.parts())
            self.assertEqual(path.read_bytes(), (REPO / release.SETS / 'kick' / 'model.mkick').read_bytes())
        # unit/ holds the Rust part fixtures, not release sets.
        sets = sorted(p.name for p in (REPO / release.SETS).iterdir() if p.is_dir() and p.name != 'unit')
        self.assertEqual(sets, sorted(SET_NAMES))
        for name in SET_NAMES:
            model = kc.read(REPO / release.SETS / name / 'model.mkick')
            contract.check(model, release.parts())
            clips = sorted((REPO / release.SETS / name).glob('clip_*.mkick'))
            self.assertTrue(clips, name)
            for c in clips:
                self.assertEqual(bytes(kc.read(c)['train_id']), bytes(model['train_id']), c.name)

    def test_contract_refuses_bad_entries(self):
        """One mutated container per refusal in section 4, each refused under its own name."""
        from tools.audio_analysis import kick_container as kc
        from tools.audio_analysis.lab import contract, release
        pts = release.parts()
        base = {n: kc.read(REPO / release.SETS / n / 'model.mkick') for n in SET_NAMES}
        text = kc.text

        def without(e, *names):
            return {k: v for k, v in e.items() if k not in names}

        ck, cs, kick, snare = base['cover_kick'], base['cover_snare'], base['kick'], base['snare']
        no_stage = dict({k: v for k, v in snare.items() if not k.startswith('stage.')}, stage=np.array(0, np.int32))
        cases = [
            ('contract', without(ck, 'contract')),
            ('contract', dict(ck, contract=np.array(3, np.int32))),
            ('contract', dict(ck, contract=np.array(2, np.int64))),
            ('missing', without(ck, 'cutoff')),
            ('missing', without(ck, 'trees.offsets')),
            ('family', dict(ck, family=text('hat'))),
            ('finder', dict(ck, finder=text('rise:6'))),
            ('finder', dict(snare, finder=text('snare:loud'))),
            ('rise-db', dict(snare, **{'cand.rise_db': np.array(6.0)})),
            ('measure', dict(ck, **{'trees.measures': text('f15,tail3,prof32,fall16,res16')})),
            ('measure', dict(ck, **{'res.window': np.array(400, np.int64)})),
            ('measure', dict(ck, **{'stage.shape': text('prof32,glide3')})),
            ('channel', dict(cs, **{'net.extra': text('side')})),
            ('combine', dict(ck, combine=text('max'))),
            ('combine-models', dict(ck, combine=text('net'))),
            ('models', dict(ck, models=text('trees,net'))),
            ('f15-finder', dict(cs, **{'trees.measures': text('f15,prof32,fall16,low16')})),
            ('width', dict(ck, **{'trees.widths': np.array([15, 3, 32, 16, 15], np.int64)})),
            ('n-features', dict(ck, **{'trees.n_features': np.array(83, np.int64)})),
            ('n-features', dict(ck, **{'stage.n_features': np.array(8, np.int64)})),
            ('grid', dict(ck, rate=np.array(44100, np.int64))),
            ('grid', dict(ck, **{'net.hop': np.array(512, np.int64)})),
            ('grid', dict(kick, **{'base.sample_rate': np.array(44100, np.int64)})),
            ('windows', dict(ck, **{'stage.windows_s': np.array([1.0, 2, 3, 4, 5])})),
            ('windows', dict(ck, **{'stage.windows_s': np.zeros(0)})),
            ('level-column', dict(ck, **{'stage.level': text('f15:15')})),
            ('gate-cuts', without(ck, 'stage.cut_stage')),
            ('gate-cuts', dict(kick, **{'stage.gate': np.array(0.0)})),
            ('fitted', dict(kick, **{'stage.shape': text('prof32,low16,tmpl3')})),
            ('unknown-entry', dict(kick, **{'fit.tmpl3.extra': np.zeros(1)})),
            ('net-extra', dict(cs, **{'net.channels': np.array(2, np.int64)})),
            ('net-count', dict(ck, **{'net.count': np.array(0, np.int64)})),
            ('net-weights', dict(ck, **{'net.count': np.array(1, np.int64)})),
            ('net-weights', without(ck, 'net.1.head.2.bias')),
            ('net-shapes', dict(ck, **{'net.1.head.2.bias': np.zeros(2, np.float32)})),
            ('unknown-entry', dict(ck, **{'extra.nope': np.zeros(1)})),
            ('unknown-entry', dict(ck, whatever=np.zeros(1))),
            ('unknown-entry', dict(snare, **{'base.hop': np.array(256, np.int64)})),
            ('unknown-entry', dict(no_stage, **{'stage.gate': np.array(0.0)})),
        ]
        for case, e in cases:
            with self.assertRaises(contract.ContractError, msg=case) as cm:
                contract.check(e, pts)
            self.assertEqual(cm.exception.case, case, str(cm.exception))
        for n, e in base.items():
            contract.check(e, pts)

    def test_release_cutoffs_from_outer(self):
        """A release's cutoffs come from each song's outer prediction (models that never saw its group): a synthetic
        run whose outer, pair and in-sample predictions differ picks the outer ones, and the stage's cutoffs come from
        its ungated outer probabilities, gated with the detector's and the stage's own cuts."""
        from scipy.special import logit

        from tools.audio_analysis.lab import release, stage
        cfg = config.load('kick_gen50_gated')
        lab = {t: {} for t in 'abcdefgh'}
        songs = {t: type('S', (), dict(kind='project', gain_db=None, parent=None, probe=False))() for t in lab}
        grps, trainings, _ = models.plan(cfg, lab, songs)
        meta = dict(groups=grps, trainings=trainings)
        rng = np.random.default_rng(7)
        metas, good, insample, inputs, raw = {}, {}, {}, {}, {}
        n = 300
        for t in lab:
            avail = np.sort(rng.choice(np.arange(100, 11000), n, replace=False))
            emit = (avail + 1) * 256 / 48000
            y = rng.uniform(size=n) < .3
            s = dict(positives=list(emit[y]), hard_neg=[], unscored=[], loop_spans=[], duration_s=60.0, positives_only=False)
            metas[t] = (s, emit, avail, 48000, 256)
            good[t] = np.clip(.35 + .4 * y + rng.normal(0, .2, n), .001, .999)
            insample[t] = good[t] ** .3
            raw[t] = np.clip(.2 + .5 * y + rng.normal(0, .25, n), .001, .999)
            inputs[t] = np.column_stack([logit(good[t]), np.zeros((n, 3)), np.log1p(rng.uniform(0, 2, n))])
        z = {}
        for i, tr in enumerate(trainings):
            for t, pre in models.save_keys(grps, tr).items():
                z[f'{pre}__combined__{t}' if pre else f'combined__{t}'] = good[t] if not pre else rng.uniform(size=n)
        outer = release.outer_of(meta, release.run_layers(cfg, meta, [z]))
        self.assertEqual(sorted(outer), sorted(lab))
        for t in lab:
            self.assertIs(outer[t], good[t])
        st = dict(cfg.stage)
        cutoff, cuts = release.release_cutoffs(st, metas, outer, inputs, raw)
        want = (models.cutoff(metas, good), models.cutoff(metas, raw))
        self.assertEqual(cuts, want)
        self.assertEqual(cutoff, models.cutoff(metas, {t: stage.gated(st, inputs[t], raw[t], want) for t in lab}))
        self.assertEqual(release.release_cutoffs(None, metas, outer), (want[0], None))
        # The test can fail: in-sample predictions choose another cutoff.
        self.assertNotEqual(models.cutoff(metas, insample), want[0])
        # Gate off: the cuts are unused and the cutoff is the stage's own.
        self.assertEqual(release.release_cutoffs(dict(st, gate=0), metas, outer, inputs, raw), (want[1], None))

    def test_release_averages_like_runtime(self):
        """Several nets average one way everywhere (D17): the release's outer layer from saved seeds, the reference's
        p.net in the coverage goldens (two nets each) and the combine on top, all expit of the mean clipped logit of
        each net's f32 sigmoid. One net passes through untouched."""
        import torch
        from scipy.special import expit, logit

        from tools.audio_analysis import kick_container as kc
        from tools.audio_analysis.lab import release

        def lg(p):
            return logit(np.clip(p, 1e-6, 1 - 1e-6))
        cfg = config.load('cover_kick')
        lab = {t: {} for t in 'abcdef'}
        songs = {t: type('S', (), dict(kind='project', gain_db=None, parent=None, probe=False))() for t in lab}
        grps, trainings, _ = models.plan(cfg, lab, songs)
        meta = dict(groups=grps, trainings=trainings)
        rng = np.random.default_rng(5)
        zs = [{}, {}]
        for tr in trainings:
            for t, pre in models.save_keys(grps, tr).items():
                for z in zs:
                    for layer in ('net', 'trees', 'combined'):
                        z[f'{pre}__{layer}__{t}' if pre else f'{layer}__{t}'] = rng.uniform(0, 1, 40)
        two = release.run_layers(cfg, meta, zs)
        one = release.run_layers(cfg, meta, zs[:1])
        for i, tr in enumerate(trainings):
            for t, pre in models.save_keys(grps, tr).items():
                key = (lambda m: f'{pre}__{m}__{t}' if pre else f'{m}__{t}')
                net = expit((lg(zs[0][key('net')]) + lg(zs[1][key('net')])) / 2)
                np.testing.assert_allclose(two[i][t], expit((lg(net) + lg(zs[0][key('trees')])) / 2), rtol=0, atol=1e-14)
                self.assertIs(one[i][t], zs[0][key('combined')])
        for name in ('cover_kick', 'cover_snare'):
            g = kc.read(REPO / release.SETS / name / 'clip_dense.mkick')
            self.assertEqual(g['net.logit'].shape[1], 2, name)
            ps = [torch.sigmoid(torch.from_numpy(np.ascontiguousarray(g['net.logit'][:, j]))).numpy().astype(np.float64)
                  for j in range(2)]
            np.testing.assert_allclose(g['p.net'], expit((lg(ps[0]) + lg(ps[1])) / 2), rtol=0, atol=1e-14, err_msg=name)
            np.testing.assert_allclose(g['p.combined'], expit((lg(g['p.net']) + lg(g['p.trees'])) / 2), rtol=0, atol=1e-14,
                                       err_msg=name)
        p = rng.uniform(size=9)
        self.assertIs(models.mean_logit_of([p]), p)

    def test_release_refuses_undeclared_part(self):
        """A config using a part the app does not run stops the release before anything trains; the chosen recipes
        and the coverage configs pass."""
        from unittest import mock

        from tools.audio_analysis.lab import release
        for name in ('kick_gen50_gated', 'snare_gen50_hybrid_low16', 'kick_v2_final', 'snare_v2_mid', 'cover_kick', 'cover_snare'):
            release.refuse_undeclared(config.load(name))
        s = config.load('snare_gen50_hybrid_low16')

        def edit(cfg, **kw):
            return config.Experiment(**{**cfg.__dict__, **kw})
        trees = s.models['trees']
        bad = [edit(s, models=dict(s.models, trees=dict(trees, measures=['prof32', 'res16']))),
               edit(s, models=dict(s.models, trees=dict(trees, measures=['glide3']))),
               edit(s, models=dict(s.models, trees=dict(trees, measures=['f15', 'prof32']))),
               edit(s, models=dict(s.models, trees=dict(trees, measures=['pitch6']))),
               edit(s, stage=dict(s.stage, windows_s=[1.0, 2.0, 3.0, 4.0, 5.0])),
               edit(s, stage=dict(s.stage, shape=['res16'])),
               edit(s, candidates='rise:6'),
               config.load('kick_gen50_gated_t8')]
        for cfg in bad:
            with self.assertRaises(SystemExit, msg=cfg):
                release.refuse_undeclared(cfg)
        with mock.patch.object(release, 'train_final', side_effect=AssertionError('trained')), \
                mock.patch.object(release, 'train_provisional', side_effect=AssertionError('trained')):
            with self.assertRaises(SystemExit):
                release.train('kick_gen50_gated_t8', 0, 1, False)

    def test_coverage_sets_fire_and_gate(self):
        """The coverage goldens exercise the fire rule and the gate: every clip fires, the detector's and the stage's
        cuts differ, p.final is the gate of p.stage under those cuts, and swapping the cuts changes it, so a runtime
        that mixes them up fails parity."""
        from tools.audio_analysis import kick_container as kc
        from tools.audio_analysis.lab import release, stage
        for name in ('cover_kick', 'cover_snare'):
            model = kc.read(REPO / release.SETS / name / 'model.mkick')
            gate = float(model['stage.gate'])
            cuts = (float(model['stage.cut_detector']), float(model['stage.cut_stage']))
            self.assertGreater(gate, 0, name)
            self.assertNotEqual(cuts[0], cuts[1], name)
            st = dict(gate=gate, level=bool(len(model['stage.level'])))
            clips = sorted((REPO / release.SETS / name).glob('clip_*.mkick'))
            self.assertTrue(clips, name)
            for c in clips:
                g = kc.read(c)
                self.assertGreater(len(g['fires']), 0, f'{name} {c.name}')
                np.testing.assert_allclose(stage.gated(st, g['stage.x'], g['p.stage'], cuts), g['p.final'], rtol=0, atol=1e-15,
                                           err_msg=f'{name} {c.name}')
                self.assertFalse(np.allclose(stage.gated(st, g['stage.x'], g['p.stage'], cuts[::-1]), g['p.final'],
                                             rtol=0, atol=1e-6), f'{name} {c.name}: swapped cuts give the same p.final')

    def test_runtime_parts_exist_in_the_lab(self):
        """runtime_parts.json declares the doc's parts, each exists in the lab registries with its declared width, and
        a wrong width, an unknown measure or channel is caught."""
        from tools.audio_analysis.lab import release
        pts = release.parts()
        self.assertEqual(pts['finders'], ['kick', 'snare'])
        self.assertEqual(pts['measures'], dict(f15=15, tail3=3, tmpl3=3, prof32=32, fall16=16, low16=16, cdom4=4,
                                               note13=13, kick2=2))
        self.assertEqual((pts['channels'], pts['combines']), (['flat', 'perc'], ['net', 'trees', 'mean_logit']))
        self.assertEqual(release.check_parts(pts), [])
        for bad in (dict(pts, measures=dict(pts['measures'], prof32=31)), dict(pts, measures=dict(pts['measures'], nope=3)),
                    dict(pts, channels=['flat', 'side']), dict(pts, finders=['kick', 'rise']),
                    dict(pts, stage=dict(pts['stage'], max_windows=5))):
            self.assertTrue(release.check_parts(bad), bad)


class FileBackedSpectrumTests(unittest.TestCase):
    """BUG-xd15a: the spectrum artifact is file-backed (a raw .npy beside the bundle, memory-mapped into the Song),
    so Songs hold file-backed pages the kernel can evict without swapping instead of anonymous RAM. The cache key,
    stamps and .npz bundles are unchanged; an existing cache is migrated by widening its own .npz copy."""

    def _stub(self, d, name, blob):
        """A registry Song whose content is only these bytes (the test_recycled_name pattern)."""
        p = Path(d) / f'{name}.wav'
        p.write_bytes(blob)
        return song(f'fb_{name}', p)

    def _spec16(self, seed):
        rng = np.random.default_rng(seed)
        return (rng.normal(0, 20, (600, 64)) + 20).astype(np.float16)

    def test_legacy_cache_reads_backed_identically(self):
        """An existing .npz cache is still the hit: its first file-backed read is migrated to a .npy sidecar widened
        from it, the view holds the identical float32 bytes, the builder is not re-run, and the bundle files keep
        their names and bytes."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            root = d / 'c'
            stub = self._stub(d, 'one', b'backed-fixture-one')
            spec16 = self._spec16(1)
            builds = []

            def build():
                builds.append(1)
                return spec16.copy()
            old = cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root)
            self.assertEqual(builds, [1])
            names = sorted(p.name for p in root.iterdir())
            hashes = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in root.iterdir()}
            backed = cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root, file_backed=True)
            self.assertEqual(builds, [1], 'the migration must not rebuild')
            npy_name = next(n for n in names if n.endswith('.npz')).replace('.npz', '.npy')
            self.assertEqual(sorted(p.name for p in root.iterdir()), sorted([*names, npy_name]),
                             'the sidecar joins the existing bundle files, none of which moves')
            self.assertTrue(np.array_equal(np.asarray(backed), old.astype(np.float32)),
                            'the mmap view must hold the same bytes the legacy npz path returned')
            self.assertEqual(backed.dtype, np.float32)
            self.assertTrue(backed.flags.writeable, 'torch.from_numpy needs a writeable view')
            after = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in root.iterdir()
                     if p.suffix in ('.json', '.npz')}
            self.assertEqual(after, {k: v for k, v in hashes.items() if k.endswith(('.json', '.npz'))},
                             'the bundle is untouched by the migration')

    def test_backed_hit_is_a_view_and_rebuilds_nothing(self):
        """The first file-backed read builds the artifact, saves the sidecar .npy and returns only a view of it; a
        second read serves the same view without rebuilding, and the legacy read still returns the float16 payload."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            root = d / 'c'
            stub = self._stub(d, 'two', b'backed-fixture-two')
            spec16 = self._spec16(2)
            builds = []

            def build():
                builds.append(1)
                return spec16.copy()
            first = cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root, file_backed=True)
            self.assertEqual(builds, [1])
            self.assertIsInstance(first, np.memmap)
            second = cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root, file_backed=True)
            self.assertEqual(builds, [1])
            self.assertTrue(np.array_equal(np.asarray(second), np.asarray(first)))
            legacy = cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root)
            self.assertEqual(legacy.dtype, np.float16)
            self.assertTrue(np.array_equal(legacy.astype(np.float32), np.asarray(first)))

    def test_backed_song_matches_legacy_song_slices(self):
        """A Song over the mmap cuts byte-identical slices to a Song over the RAM float32 array."""
        import torch
        from tools.audio_analysis.eval.kick_goal_nn import Song as NetSong
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            root = d / 'c'
            stub = self._stub(d, 'three', b'backed-fixture-three')
            spec16 = self._spec16(3)
            build = lambda: spec16.copy()  # noqa: E731
            rng = np.random.default_rng(4)
            onset = np.sort(rng.uniform(1.0, 3.0, 40))
            emit = onset + 0.08
            mask, y, hard = np.ones(40, bool), rng.integers(0, 2, 40), np.zeros(40, bool)
            idx, shift = np.arange(6), rng.integers(-1, 2, 6)
            ram = NetSong(cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root).astype(np.float32),
                          onset, emit, mask, y, 'fb', hard)
            backed = NetSong(cache.artifact(stub, 'spectrum', dict(bands=64), build, root=root, file_backed=True),
                             onset, emit, mask, y, 'fb', hard)
            self.assertTrue(bool(ram.slices(idx, shift).equal(backed.slices(idx, shift))),
                            'a Song over the mmap must cut the same slices as a Song over the RAM array')


class _Draws:
    """A default_rng stand-in for the train() sampling block that records every draw it makes."""

    def __init__(self, gen):
        self.gen, self.calls = gen, []

    def choice(self, idx, draws, p=None):
        pick = self.gen.choice(idx, draws, p=p)
        self.calls.append(('choice', tuple(int(x) for x in pick)))
        return pick

    def integers(self, *a, **k):
        out = self.gen.integers(*a, **k)
        self.calls.append(('integers', tuple(int(x) for x in np.asarray(out).ravel())))
        return out

    def permutation(self, n):
        out = self.gen.permutation(n)
        self.calls.append(('permutation', tuple(int(x) for x in out)))
        return out


class TrainMathBatteryTests(unittest.TestCase):
    """The sampling arithmetic is untouched by the on-disk edits: the drawn indices, the jitter and the batch
    permutation, the cut slices, and a one-step gradient all match git HEAD's train() exactly, on tiny fixtures and
    the CPU device only (no GPU, no MPS)."""

    _HEAD = None

    @classmethod
    def _head_nn(cls):
        """The kick_goal_nn module as git HEAD has it, executed from a scratch copy (never the worktree's file)."""
        if cls._HEAD is None:
            import importlib.util
            import subprocess
            src = subprocess.run(['git', '-C', str(Path(__file__).resolve().parents[3]),
                                  'show', 'HEAD:tools/audio_analysis/eval/kick_goal_nn.py'],
                                 check=True, capture_output=True, text=True).stdout
            scratch = Path(tempfile.mkdtemp()) / 'kick_goal_nn_head.py'
            scratch.write_text(src)
            mod_spec = importlib.util.spec_from_file_location('kick_goal_nn_head', scratch)
            head = importlib.util.module_from_spec(mod_spec)
            mod_spec.loader.exec_module(head)
            cls._HEAD = head
        return cls._HEAD

    @staticmethod
    def _nn():
        from tools.audio_analysis.eval import kick_goal_nn
        return kick_goal_nn

    @staticmethod
    def _tiny_songs():
        """Two Songs, each a small spectrum with a few dozen training rows and one extra channel (no clips)."""
        from tools.audio_analysis.eval.kick_goal_nn import Song
        out = []
        for k in range(2):
            rng = np.random.default_rng(10 + k)
            n = 2000
            spec = (rng.normal(0, 20, (n, 64)) + 20).astype(np.float32)
            onset = np.sort(rng.uniform(1.0, 3.0, 60))
            emit = onset + 0.08
            mask, y = np.ones(60, bool), rng.integers(0, 2, 60)
            hard = rng.integers(0, 2, 60) == 1
            extra = rng.normal(0, 1, (n, 64)).astype(np.float32)
            out.append(Song(spec, onset, emit, mask, y, f't{k}', hard, (extra,)))
        return out

    @staticmethod
    def _train_once(mod, songs, seed):
        """train() one epoch with the default_rng behind a recorder, so every draw is captured; returns the trained
        state dict and the draws."""
        real, recs = np.random.default_rng, []

        def recorded(s, *a, **k):
            wrap = _Draws(real(s, *a, **k))
            recs.append(wrap)
            return wrap
        want = os.environ.get('KICK_GOAL_NN_DEVICE')
        os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
        np.random.default_rng = recorded
        try:
            net = mod.train(songs, seed, epochs=1, batch=10 ** 9)
        finally:
            np.random.default_rng = real
            if want is None:
                os.environ.pop('KICK_GOAL_NN_DEVICE', None)
            else:
                os.environ['KICK_GOAL_NN_DEVICE'] = want
        return net.state_dict(), recs[0].calls

    def test_train_sampling_matches_head(self):
        """For one seed, HEAD and current train() draw the same indices, the same jitter and the same batch
        permutation (the same numpy stream over the same fixture)."""
        _, head_draws = self._train_once(self._head_nn(), self._tiny_songs(), 5)
        _, now_draws = self._train_once(self._nn(), self._tiny_songs(), 5)
        self.assertTrue(head_draws, 'the sampling must have drawn')
        self.assertEqual(head_draws, now_draws)

    def test_unit_row_weights_leave_the_stream_alone(self):
        """sampling.hard_negatives / coincident_weight off is the HEAD stream (the test above); weights of all ones
        are the same draws, so the option only moves the rows it reweights."""
        _, head_draws = self._train_once(self._head_nn(), self._tiny_songs(), 5)
        songs = self._tiny_songs()
        for s in songs:
            s.neg_w, s.pos_w = np.ones(len(s.y)), np.ones(len(s.y))
        _, ones_draws = self._train_once(self._nn(), songs, 5)
        self.assertEqual(head_draws, ones_draws)
        songs = self._tiny_songs()
        songs[0].neg_w = np.where(np.arange(60) % 2 == 0, 5.0, 1.0)
        _, moved = self._train_once(self._nn(), songs, 5)
        self.assertNotEqual(head_draws, moved)

    def test_row_weights_set_the_draw_probabilities(self):
        """draw_weights: the vouched-negative weight times neg_w for negatives, pos_w for positives, each untouched by
        the other; and train() draws only the candidates a weight leaves alive."""
        nn = self._nn()
        song = self._tiny_songs()[0]
        idx = np.arange(60)
        hard = np.where(song.hard[idx], nn.HARD_W, 1.0)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 0.0), hard)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 1.0), np.ones(60))
        song.neg_w = np.linspace(1.0, 7.0, 60)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 0.0), hard * song.neg_w)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 1.0), np.ones(60))
        song.pos_w = np.linspace(2.0, 3.0, 60)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 1.0), song.pos_w)
        np.testing.assert_array_equal(nn.draw_weights(song, idx, 0.0), hard * song.neg_w)
        # In train(): only one negative and one positive of song t0 stay alive, so every draw of that class is it.
        songs = self._tiny_songs()
        pos, neg = np.flatnonzero(songs[0].mask & (songs[0].y == 1)), np.flatnonzero(songs[0].mask & (songs[0].y == 0))
        songs[0].neg_w, songs[0].pos_w = np.zeros(60), np.zeros(60)
        songs[0].neg_w[neg[3]], songs[0].pos_w[pos[2]] = 1.0, 1.0
        _, calls = self._train_once(self._nn(), songs, 5)
        picks = [c[1] for c in calls if c[0] == 'choice']   # song t0: positives, negatives; then song t1's
        self.assertEqual(set(picks[0]), {int(pos[2])})
        self.assertEqual(set(picks[1]), {int(neg[3])})
        self.assertGreater(len(set(picks[2])), 1)

    def test_train_one_step_matches_head(self):
        """One forward/backward on a tiny batch, weights fixed by the seed: the trained state is byte-identical to
        the HEAD-materialized training's."""
        head_state, _ = self._train_once(self._head_nn(), self._tiny_songs(), 7)
        now_state, _ = self._train_once(self._nn(), self._tiny_songs(), 7)
        self.assertEqual(sorted(head_state), sorted(now_state))
        for k, v in head_state.items():
            np.testing.assert_array_equal(v.numpy(), now_state[k].numpy(), err_msg=k)

    def test_slices_cut_then_moved_equal_cut_on_cpu(self):
        """On the CPU: a song cut after .to(cpu) cuts exactly what it cut before the move, and the per-batch move
        leaves the values alone (the train() batch-layout change is arithmetic-free on this device)."""
        import torch
        song0 = self._tiny_songs()[0]
        idx, shift = np.arange(0, 12), np.array([1, -1, 0, 1, -1, 0, 1, -1, 0, 1, -1, 0])
        cut = song0.slices(idx, shift)
        moved = song0.to(torch.device('cpu')).slices(idx, shift)
        self.assertTrue(bool(cut.equal(moved)))
        self.assertTrue(bool(cut.equal(cut.to(torch.device('cpu')))))


class ModelKeyScopeTests(unittest.TestCase):
    """BUG-4x7jh: one model's key moves with what can change its saved layer, and with nothing else."""

    CFG = 'kick_blend'   # both models; no probing settings of its own

    def _meta(self):
        trainings = [dict(without=[0], held=['a'], trained=['b', 'c'], clips=[]),
                     dict(without=[1], held=['b'], trained=['a', 'c'], clips=[])]
        return dict(parts=dict(finder='f', frames='fm', measures='m'), trainings=trainings,
                    label_ids=dict(a='1', b='2', c='3', p='7'), content=dict(a='A', b='B', c='C', p='P'))

    def test_model_key_ignores_songs_it_never_touches(self):
        """A song outside every training (a new generated song nobody fits or predicts) moves no model's key; a
        held song's audio still does, because the layer npz stores its predictions."""
        cfg = config.load(self.CFG)
        base = {m: run.model_key(cfg, m, self._meta()) for m in cfg.models}
        wide = self._meta()
        wide['content'] = dict(wide['content'], p='P2')
        wide['label_ids'] = dict(wide['label_ids'], p='8')
        self.assertEqual({m: run.model_key(cfg, m, wide) for m in cfg.models}, base)
        who_moves = {m: run.model_key(cfg, m, dict(self._meta(), content=dict(self._meta()['content'], a='A2')))
                     for m in cfg.models}
        self.assertNotEqual(who_moves, base)

    def test_net_code_ignores_the_trees_fitting(self):
        """BUG-4x7jh: the net's code term covers only the net's training path, so a change to models.fit_trees (even
        a comment) moves the trees' key and not the net's."""
        from tools.audio_analysis.lab import models
        cfg = config.load(self.CFG)
        meta = self._meta()
        base = {m: run.model_key(cfg, m, meta) for m in cfg.models}
        real = models.fit_trees

        def another_fit(*a, **k):
            return real(*a, **k)
        models.fit_trees = another_fit
        try:
            now = {m: run.model_key(cfg, m, meta) for m in cfg.models}
        finally:
            models.fit_trees = real
        self.assertEqual(now['net'], base['net'])
        self.assertNotEqual(now['trees'], base['trees'])

    def test_trees_code_ignores_the_net_training(self):
        """The trees' code term covers only the trees path (models.fit_trees, tree_rows): a change to
        kick_goal_nn.train moves the net's key and not the trees'."""
        from tools.audio_analysis.eval import kick_goal_nn
        cfg = config.load(self.CFG)
        meta = self._meta()
        base = {m: run.model_key(cfg, m, meta) for m in cfg.models}
        real = kick_goal_nn.train

        def another_train(*a, **k):
            return real(*a, **k)
        kick_goal_nn.train = another_train
        try:
            now = {m: run.model_key(cfg, m, meta) for m in cfg.models}
        finally:
            kick_goal_nn.train = real
        self.assertNotEqual(now['net'], base['net'])
        self.assertEqual(now['trees'], base['trees'])

    def test_sampling_probes_leave_the_model_keys(self):
        """BUG-4x7jh: the sampling.gain_probes setting rides the run's scoring, not a model's key; the probe copies
        themselves still do, as held songs (the layer npz stores their predictions)."""
        base = config.load(self.CFG)
        probed = config.Experiment(**{**base.__dict__, 'sampling': {**base.sampling, 'gain_probes': [-6.0, 6.0]}})
        self.assertEqual({m: run.model_key(probed, m, self._meta()) for m in probed.models},
                         {m: run.model_key(base, m, self._meta()) for m in base.models})
        meta = self._meta()
        meta['trainings'] = [dict(n, held=n['held'] + ['p']) for n in meta['trainings']]
        self.assertNotEqual({m: run.model_key(probed, m, meta) for m in probed.models},
                            {m: run.model_key(base, m, self._meta()) for m in base.models})

    def test_plan_matches_the_imported_run(self):
        """BUG-1yrqo: run.py's --plan path in its own process derives the same net and trees model keys the imported
        run does, so imported-vs-__main__ env drift (recipe or import order) cannot silently fork the keys. --plan
        only the id and nothing trains."""
        import subprocess
        cfg = config.load('kick_v2_final')
        lab, songs, meta = run.setup(cfg)
        want = {m: run.model_key(cfg, m, meta) for m in cfg.models}
        prog = ('import json, runpy, sys\n'
                'sys.argv = ["run.py", "--plan", "kick_v2_final"]\n'
                f'runpy.run_path({str(Path(run.__file__).resolve())!r}, run_name="__main__")\n'
                'from tools.audio_analysis.lab import config, run\n'
                'cfg = config.load(sys.argv[2])\n'
                'lab, songs, meta = run.setup(cfg)\n'
                'keys = {m: run.model_key(cfg, m, meta) for m in cfg.models}\n'
                'print(json.dumps(keys))\n')
        r = subprocess.run([sys.executable, '-P', '-c', prog], capture_output=True, text=True,
                           cwd=str(Path(tempfile.gettempdir())))
        self.assertEqual(r.returncode, 0, r.stderr[-2000:])
        self.assertEqual(json.loads(r.stdout.strip().splitlines()[-1]), want)


if __name__ == '__main__':
    unittest.main()
