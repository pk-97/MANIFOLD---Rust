#!/usr/bin/env python3
"""The overnight preflight's tests (fake machine, fake plan, fake verdict store; no GPU, no training).
Run directly: test_preflight.py"""
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.lab import cache, kick_verdict, overnight, preflight  # noqa: E402

PLAN = dict(name='night', steps=[
    dict(run='kick_a', args=['--seeds', '0,1']),
    dict(run='snare_a'),
    dict(compare=['snare_a', 'snare_b'], args=['--exam-of', 'snare-old']),
    dict(compare=['kick_a', 'kick_b'], release=True),
    dict(release='snare_a', args=['--seed', '0', '--nets', '3']),
])


class FakeProbe:
    """A machine where everything is fine; each test spoils one thing."""

    def __init__(self, runs):
        self.runs = runs
        self.dirty, self.mps_ok, self.free, self.broken = [], (True, ''), 80e9, {}
        self.misses = {}                 # (config, release) -> [(song, artifact)]
        self.keys = {'ida.p', 'idb.p'}   # the fake verdict store
        self.gpu_state = (True, 'GPU free', True)
        self.inspected = []

    def git_dirty(self):
        return self.dirty

    def mps(self):
        return self.mps_ok

    def free_bytes(self):
        return self.free

    def run_id(self, config, release):
        if config in self.broken:
            return False, self.broken[config]
        return True, f'{config.split("_")[0]}-{config}{"-rel" if release else ""}'

    def inspect(self, config, release):
        self.inspected.append((config, release))
        return dict(family=config.split('_')[0], songs={'sa': 'ida', 'sb': 'idb'},
                    misses=self.misses.get((config, release), []))

    def verdict_store(self):
        return Path('kick-X.npz'), self.keys

    def gpu(self):
        return self.gpu_state

    def memory(self):
        return 36 * 2 ** 30, 2153167, 1955.4


class PreflightTests(unittest.TestCase):
    def setUp(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.dir = Path(d.name)
        self.plan = self.dir / 'plan.json'
        self.plan.write_text(json.dumps(PLAN))
        (self.dir / 'runs/snare-old').mkdir(parents=True)
        (self.dir / 'runs/snare-old/meta.json').write_text(json.dumps(dict(exam=['sa', 'sb'])))
        self.probe = FakeProbe(self.dir / 'runs')

    def rows(self, plan=None):
        return preflight.check(plan or self.plan, self.probe, overnight.load_plan)

    def verdicts(self, rows):
        return [r[0] for r in rows]

    def test_everything_fine_passes_all_ten_in_order(self):
        rows = self.rows()
        self.assertEqual(self.verdicts(rows), [True] * 10)
        text = '\n'.join(preflight.render(rows))
        self.assertIn('PASS 4. configs resolve: kick_a -> kick-kick_a, snare_a -> snare-snare_a', text)
        self.assertIn('kick_a --release -> kick-kick_a-rel', text)
        self.assertNotIn('FAIL', text)
        self.assertIn('PASS 10. memory: 36 GB RAM, swap used 1955 MB, 2153167 pageouts since boot; the queue kills a '
                      'step when pageouts rise at every 5 s sample for 60 s', text)

    def test_unreadable_memory_counters_fail(self):
        def broken():
            raise RuntimeError('cannot read paging counters')
        self.probe.memory = broken
        rows = self.rows()
        self.assertFalse(rows[9][0])
        self.assertEqual(self.verdicts(rows)[:9], [True] * 9)

    def test_configs_are_deduped_and_release_flag_is_part_of_the_key(self):
        cfgs, exams = preflight.configs_of(overnight.load_plan(self.plan)[1])
        self.assertEqual(cfgs, [('kick_a', False), ('snare_a', False), ('snare_b', False), ('kick_a', True),
                                ('kick_b', True)])
        self.assertEqual(exams, ['snare-old'])

    def test_dirty_tree_mps_and_disk_fail(self):
        self.probe.dirty, self.probe.mps_ok, self.probe.free = [' M a.py'], (False, 'no MPS'), 10e9
        rows = self.rows()
        self.assertEqual(self.verdicts(rows)[:3], [False] * 3)
        self.assertEqual(rows[0][3], [' M a.py'])
        self.assertIn('no MPS', rows[1][2])
        self.assertIn('10 GB free', rows[2][2])

    def test_unresolvable_config_fails_check_4_and_is_not_inspected(self):
        self.probe.broken = {'snare_b': 'no such config'}
        rows = self.rows()
        self.assertFalse(rows[3][0])
        self.assertEqual(rows[3][3], ['snare_b: no such config'])
        self.assertNotIn(('snare_b', False), self.probe.inspected)

    def test_kick_verdicts_are_checked_for_snare_configs_only(self):
        self.probe.keys = {'ida.p'}
        rows = self.rows()
        self.assertFalse(rows[4][0])
        self.assertEqual(sorted(rows[4][3]), ['snare_a: 1 song(s): sb', 'snare_b: 1 song(s): sb'])
        self.probe.keys = set()
        self.probe.inspect = lambda c, r: dict(family='kick', songs={'sa': 'ida'}, misses=[])
        self.assertTrue(self.rows()[4][0])   # no snare-family config: nothing to look up

    def test_missing_verdict_store_fails(self):
        def gone():
            raise FileNotFoundError('no kick verdict store kick-X.npz')
        self.probe.verdict_store = gone
        self.assertIn('no kick verdict store', self.rows()[4][2])

    def test_exam_of_needs_meta_with_an_exam_list(self):
        (self.dir / 'runs/snare-old/meta.json').write_text(json.dumps(dict(songs=['sa'])))
        rows = self.rows()
        self.assertFalse(rows[5][0])
        self.assertIn('has no exam list', rows[5][3][0])
        (self.dir / 'runs/snare-old/meta.json').unlink()
        self.assertIn('snare-old: no ', self.rows()[5][3][0])

    def test_cold_cache_names_the_song_and_artifact(self):
        self.probe.misses = {('snare_a', False): [('sa', 'candidates'), ('sb', 'candidates'), ('sb', 'spectrum')]}
        rows = self.rows()
        self.assertFalse(rows[6][0])
        self.assertEqual(rows[6][3], ['snare_a: candidates missing for 2 song(s): sa, sb',
                                      'snare_a: spectrum missing for 1 song(s): sb'])

    def test_gpu_lock_busy_with_live_or_dead_holder_fails(self):
        self.probe.gpu_state = (False, 'GPU busy: pid 5 `x`', True)
        self.assertIn('would wait behind it', self.rows()[7][2])
        self.probe.gpu_state = (False, 'GPU busy: pid 5 `x`', False)
        rows = self.rows()
        self.assertFalse(rows[7][0])
        self.assertIn('holder process is dead', rows[7][2])

    def test_bad_plan_fails_check_9_and_leaves_no_config_to_check(self):
        bad = self.dir / 'bad.json'
        bad.write_text(json.dumps(dict(steps=[dict(run='a', compare=['b', 'c'])])))
        rows = self.rows(bad)
        self.assertFalse(rows[8][0])
        self.assertIn('exactly one of', rows[8][2])
        self.assertEqual(self.probe.inspected, [])
        self.assertIn('no plan to read', rows[3][2])

    def test_render_lists_details_under_the_line(self):
        self.probe.dirty = [' M a.py']
        lines = preflight.render(self.rows())
        self.assertTrue(lines[0].startswith('FAIL 1. '))
        self.assertEqual(lines[1].strip(), 'M a.py')


class PeekTests(unittest.TestCase):
    """The lookup check 7 swaps in agrees with the real cache and writes nothing."""

    def test_hit_after_a_real_build_miss_otherwise_and_no_files_written(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        song = SimpleNamespace(id='s', content_id='c' * 64)
        settings = dict(rule='x')
        cache.artifact(song, 'candidates', settings, lambda: np.zeros((2, 3), np.int64), root=root)
        cache.artifact(song, 'spectrum', settings, lambda: np.zeros((4, 2), np.float16), root=root, file_backed=True)
        before = sorted(p.name for p in root.iterdir())
        misses = []
        with preflight.peeking(misses):
            cache.artifact(song, 'candidates', settings, None, root=root)
            cache.artifact(song, 'spectrum', settings, None, root=root, file_backed=True)
            cache.artifact(song, 'candidates', dict(rule='y'), None, root=root)   # other settings: a miss
            cache.artifact(song, 'flatness', settings, None, root=root)           # never built: a miss
            cache.artifact(SimpleNamespace(id='t', content_id='d' * 64), 'candidates', settings, None, root=root)
        self.assertEqual(misses, [('s', 'candidates'), ('s', 'flatness'), ('t', 'candidates')])
        self.assertEqual(sorted(p.name for p in root.iterdir()), before)
        self.assertEqual(cache.artifact.__name__, 'artifact')   # restored

    def test_sidecar_alone_is_a_hit_for_a_file_backed_artifact(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        song = SimpleNamespace(id='s', content_id='c' * 64)
        cache.artifact(song, 'spectrum', {}, lambda: np.zeros((4, 2), np.float16), root=root, file_backed=True)
        for p in root.glob('*.json'):
            p.unlink()
        for p in root.glob('*.npz'):
            p.unlink()
        misses = []
        with preflight.peeking(misses):
            cache.artifact(song, 'spectrum', {}, None, root=root, file_backed=True)
            cache.artifact(song, 'spectrum', {}, None, root=root)   # not file-backed: needs the bundle
        self.assertEqual(misses, [('s', 'spectrum')])


class VerdictStoreTests(unittest.TestCase):
    def test_reads_keys_of_the_store_named_for_the_current_kick_release(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        saved = kick_verdict.STORE, kick_verdict.kick_release
        kick_verdict.STORE, kick_verdict.kick_release = Path(d.name), lambda: 'kick-T'
        self.addCleanup(lambda: (setattr(kick_verdict, 'STORE', saved[0]), setattr(kick_verdict, 'kick_release', saved[1])))
        with self.assertRaises(FileNotFoundError):
            preflight.Probe().verdict_store()
        np.savez(Path(d.name) / 'kick-T.npz', **{'abc.p': np.zeros(2), 'abc.onset': np.zeros(2)})
        path, keys = preflight.Probe().verdict_store()
        self.assertEqual(path.name, 'kick-T.npz')
        self.assertEqual(keys, {'abc.p', 'abc.onset'})


if __name__ == '__main__':
    unittest.main()
