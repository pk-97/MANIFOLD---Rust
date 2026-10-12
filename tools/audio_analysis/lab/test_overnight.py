#!/usr/bin/env python3
"""The overnight queue's tests (fake step runner; no GPU, no training). Run directly: test_overnight.py"""
import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.lab import overnight  # noqa: E402

COMPARE_TEXT = '''A: base (run a1, layer net) sampling {}, train [], train 1.0 min over 1 seeds
B - A over songs (hits and false fires averaged over seeds first): +1.20 F1 points, 90% interval -0.50 to +2.90
A: F1 0.8000 | mean 0.8000 ± 0.0000 | 5 songs
B: F1 0.8120 | mean 0.8120 ± 0.0000 | 5 songs
B - A over seeds (mean of each seed's pooled F1): +1.20 ± 0.00 F1 points
songs better / worse on F1: 3 / 1 (sign test p = 0.62)
coincident hits (snare labels within 20 ms of a kick label), recall:
  A: coincident 0.500 (4.0 of 8) | overall 0.900 (9.0 of 10) | 5 songs
  B: coincident 0.625 (5.0 of 8) | overall 0.900 (9.0 of 10) | 5 songs
    per song line that is not a headline
old exam | old exam (run k1): 3 of its 3 songs are scored here, out of 5 in this exam
old exam | B - A over songs (hits and false fires averaged over seeds first): +0.50 F1 points, 90% interval -1.00 to +2.00
old exam | A: F1 0.7000 | mean 0.7000 ± 0.0000 | 3 songs
old exam | songs better / worse on F1: 2 / 1 (sign test p = 1.00)
old exam | hits change per song (B - A, mean over seeds), worst first: not a headline
'''

PLAN = dict(name='night', steps=[
    dict(run='snare_base'),
    dict(run='snare_gain', args=['--seeds', '0,1']),
    dict(compare=['snare_base', 'snare_gain'], release=True),
    dict(compare=['a', 'b'], release='TRAIN7'),
    dict(release='snare_gain', args=['--seed', '0', '--nets', '3']),
])


class Fake:
    """A step runner: records each command, writes a log, exits with the scripted code for that step."""

    def __init__(self, codes=()):
        self.codes, self.cmds = list(codes), []

    def __call__(self, cmd, log):
        self.cmds.append(cmd)
        n = len(self.cmds)
        Path(log).write_text(COMPARE_TEXT if 'compare.py' in ' '.join(cmd) else f'step {n} output\n')
        return self.codes[n - 1] if n <= len(self.codes) else 0


class Clock:
    def __init__(self):
        self.t = 1_000_000.0

    def __call__(self):
        self.t += 10.0
        return self.t


class OvernightTests(unittest.TestCase):
    def go(self, codes, steps=None):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        fake = Fake(codes)
        out = Path(d.name) / 'q'
        code = overnight.run_plan('night', steps or PLAN['steps'], out, runner=fake, clock=Clock(), log=lambda m: None)
        return code, fake, out, (out / 'summary.txt').read_text()

    def test_all_pass_in_order_one_at_a_time_under_the_gpu_lock(self):
        code, fake, out, summary = self.go([])
        self.assertEqual(code, 0)
        self.assertEqual(len(fake.cmds), 5)
        for cmd in fake.cmds:
            self.assertTrue(cmd[1].endswith('scripts/gpu_queue.py'))
            self.assertEqual(cmd[cmd.index('--') + 1], sys.executable)
        scripts = [Path(c[c.index('--') + 2]).name for c in fake.cmds]
        self.assertEqual(scripts, ['run.py', 'run.py', 'compare.py', 'compare.py', 'release.py'])
        self.assertEqual(sorted(p.name for p in out.glob('*.log')),
                         ['01_run_snare_base.log', '02_run_snare_gain.log', '03_compare_snare_base_vs_snare_gain.log',
                          '04_compare_a_vs_b.log', '05_release_snare_gain.log'])
        self.assertIn('all 5 steps passed', summary)

    def test_args_and_release_pass_through(self):
        _, fake, _, _ = self.go([])
        tails = [c[c.index('--') + 3:] for c in fake.cmds]
        self.assertEqual(tails[0], ['snare_base'])
        self.assertEqual(tails[1], ['snare_gain', '--seeds', '0,1'])
        self.assertEqual(tails[2], ['snare_base', 'snare_gain', '--release'])
        self.assertEqual(tails[3], ['a', 'b', '--release', 'TRAIN7'])
        self.assertEqual(tails[4], ['train', 'snare_gain', '--seed', '0', '--nets', '3'])   # no --release flag

    def test_stops_at_first_failure_and_says_what_is_unrun(self):
        code, fake, out, summary = self.go([0, 3])
        self.assertEqual(code, 3)
        self.assertEqual(len(fake.cmds), 2)
        self.assertFalse(list(out.glob('03_*')))
        self.assertIn('STOPPED: step 2 exited 3', summary)
        self.assertIn('not run: step 3 (compare snare_base snare_gain), step 4 (compare a b), '
                      'step 5 (release snare_gain)', summary)
        self.assertIn('step 2 run snare_gain: exit 3', summary)

    def test_last_step_failure_leaves_nothing_unrun(self):
        code, _, _, summary = self.go([0, 0, 0, 0, 9])
        self.assertEqual(code, 9)
        self.assertIn('step 5 release snare_gain: exit 9', summary)
        self.assertIn('not run: none', summary)

    def test_summary_times_and_compare_headlines(self):
        _, _, _, summary = self.go([])
        # The fake clock ticks 10 s per read: a step reads it twice around its run, so each takes 10 s.
        self.assertIn(', 10 s, log 01_run_snare_base.log', summary)
        head = [ln.strip() for ln in summary.splitlines() if ln.startswith('    ')]
        self.assertIn('B - A over songs (hits and false fires averaged over seeds first): +1.20 F1 points, '
                      '90% interval -0.50 to +2.90', head)
        self.assertIn('A: F1 0.8000 | mean 0.8000 ± 0.0000 | 5 songs', head)
        self.assertIn('A: coincident 0.500 (4.0 of 8) | overall 0.900 (9.0 of 10) | 5 songs', head)
        self.assertNotIn('per song line that is not a headline', head)
        self.assertIn('old exam | old exam (run k1): 3 of its 3 songs are scored here, out of 5 in this exam', head)
        self.assertIn('old exam | A: F1 0.7000 | mean 0.7000 ± 0.0000 | 3 songs', head)
        self.assertIn('old exam | songs better / worse on F1: 2 / 1 (sign test p = 1.00)', head)
        self.assertFalse([h for h in head if 'not a headline' in h])
        run_block = summary.split('step 2')[0].split('step 1')[1]
        self.assertNotIn('F1', run_block)   # a run step has no headlines

    def test_dry_run_prints_commands_and_runs_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            plan = Path(d) / 'plan.json'
            plan.write_text(json.dumps(PLAN))
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                self.assertEqual(overnight.main([str(plan), '--dry-run']), 0)
            lines = buf.getvalue().splitlines()
            self.assertEqual(len(lines), 5)
            self.assertIn('gpu_queue.py', lines[0])
            self.assertTrue(lines[3].endswith('compare.py a b --release TRAIN7'))
            self.assertTrue(lines[4].endswith('release.py train snare_gain --seed 0 --nets 3'))
            self.assertEqual(list(Path(d).iterdir()), [plan])

    def test_bad_plans_exit_before_anything_runs(self):
        bad = [dict(steps=[]), dict(steps=[dict()]), dict(steps=[dict(run='a', compare=['a', 'b'])]),
               dict(steps=[dict(compare=['a'])]), dict(steps=[dict(run='a', retries=2)]),
               dict(steps=[dict(run='a', args='--seeds 0')]), dict(steps=[dict(release=True)]),
               dict(steps=[dict(release='a', args='--seed 0')])]
        with tempfile.TemporaryDirectory() as d:
            for i, plan in enumerate(bad):
                f = Path(d) / f'{i}.json'
                f.write_text(json.dumps(plan))
                with self.assertRaises(SystemExit, msg=str(plan)):
                    overnight.load_plan(f)


if __name__ == '__main__':
    unittest.main()
