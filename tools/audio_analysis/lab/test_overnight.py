#!/usr/bin/env python3
"""The overnight queue's tests (fake step runner; no GPU, no training). Run directly: test_overnight.py"""
import contextlib
import io
import json
import sys
import tempfile
import time
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


class PagingWatchTests(unittest.TestCase):
    def test_trips_only_after_every_sample_rose_for_the_window(self):
        w = overnight.PagingWatch(60, 100, 0)
        trips = [w.update(100 + 10 * i, 5 * i) for i in range(1, 14)]
        self.assertEqual(trips, [False] * 11 + [True, True])   # t = 60 is the 12th sample after the baseline

    def test_one_flat_sample_restarts_the_count(self):
        w = overnight.PagingWatch(60, 0, 0)
        for i in range(1, 12):
            self.assertFalse(w.update(i, 5 * i))
        self.assertFalse(w.update(11, 60))                   # no rise at t = 60
        self.assertFalse(w.update(12, 65))                   # a new run starts at t = 60
        self.assertFalse(w.update(13, 70))
        self.assertTrue(w.update(14, 120))                   # 60 s since the restart

    def test_zero_never_trips_and_a_counter_that_goes_back_restarts(self):
        w = overnight.PagingWatch(0, 0, 0)
        self.assertFalse(any(w.update(i, 100 * i) for i in range(1, 50)))
        w = overnight.PagingWatch(10, 100, 0)
        self.assertFalse(w.update(110, 5))
        self.assertFalse(w.update(50, 10))
        self.assertFalse(w.update(60, 15))     # the new run starts at t = 10
        self.assertTrue(w.update(70, 20))


class ExecuteTests(unittest.TestCase):
    """The real runner on a sleeping child, with fake machine readings."""
    SLEEP = [sys.executable, '-c', 'import time; time.sleep(30)']

    def go(self, cmd, counter, watch_s=0.15):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        n = iter(counter)
        return overnight.execute(cmd, Path(d.name) / 'log', watch_s=watch_s, poll_s=0.03,
                                 vm=lambda: (next(n), 777.0), rss=lambda pid: (3_000_000, [pid]))

    def test_sustained_paging_kills_the_step_and_reports_the_numbers(self):
        t0 = time.time()
        res = self.go(self.SLEEP, (1000 + 50 * i for i in range(10_000)))
        self.assertLess(time.time() - t0, 10)
        self.assertEqual((res['code'], res['killed']), (137, True))
        self.assertGreater(res['pageouts'], 0)
        self.assertEqual(res['swap_mb'], 777.0)
        self.assertEqual(res['peak_gb'], 3.0)

    def test_flat_counter_never_kills_however_large_the_step(self):
        res = self.go([sys.executable, '-c', 'import time; time.sleep(0.4)'], (1000 for _ in range(10_000)))
        self.assertEqual((res['code'], res['killed'], res['pageouts']), (0, False, 0))
        self.assertEqual(res['peak_gb'], 3.0)

    def test_exit_code_passes_through(self):
        self.assertEqual(self.go([sys.executable, '-c', 'raise SystemExit(3)'], (5 for _ in range(100)))['code'], 3)

    def test_vm_sample_parses_the_two_commands(self):
        class R:
            def __init__(self, out):
                self.stdout = out
        outs = {'vm_stat': 'Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPageouts:     2153167.\n',
                'sysctl': 'vm.swapusage: total = 3072.00M  used = 1955.44M  free = 1116.56M  (encrypted)\n'}
        real = overnight.subprocess.run
        overnight.subprocess.run = lambda cmd, **kw: R(outs[cmd[0]])
        try:
            self.assertEqual(overnight.vm_sample('darwin'), (2153167, 1955.44))
        finally:
            overnight.subprocess.run = real

    def test_vm_sample_reads_proc_on_linux(self):
        files = {'/proc/vmstat': 'pgpgout 98765\npswpin 120\npswpout 4567\n',
                 '/proc/meminfo': 'MemTotal:       98000000 kB\nSwapTotal:       8388604 kB\nSwapFree:        7340028 kB\n'}
        self.assertEqual(overnight.vm_sample('linux', files.get), (4567, 1024.0))


class KilledStepTests(unittest.TestCase):
    def test_a_killed_step_is_recorded_and_the_queue_goes_on(self):
        steps = PLAN['steps']

        def runner(cmd, log):
            n = len([1 for _ in runner.seen]) + 1
            runner.seen.append(cmd)
            Path(log).write_text('')
            if n == 2:
                return dict(code=137, killed=True, peak_gb=21.5, pageouts=90000, swap_mb=3000.0)
            return dict(code=0, killed=False, peak_gb=4.0, pageouts=12, swap_mb=100.0)
        runner.seen = []
        with tempfile.TemporaryDirectory() as d:
            code = overnight.run_plan('night', steps, Path(d) / 'q', runner=runner, clock=Clock(), log=lambda m: None,
                                      watch_s=60)
            summary = (Path(d) / 'q/summary.txt').read_text()
        self.assertEqual(len(runner.seen), 5)
        self.assertEqual(code, 137)
        self.assertIn('step 2 run snare_gain: exit 137 (KILLED: sustained paging, pageouts rose at every sample for 60 s), '
                      'peak 21.5 GB resident, pageouts +90000, swap used 3000 MB', summary)
        self.assertIn('step 1 run snare_base: exit 0, peak 4.0 GB resident, pageouts +12, swap used 100 MB', summary)
        self.assertIn('all 5 steps ran; killed for sustained paging: step 2 (run snare_gain)', summary)
        self.assertNotIn('STOPPED', summary)

    def test_another_failure_after_a_kill_still_stops_the_queue(self):
        codes = iter([dict(code=137, killed=True), dict(code=2, killed=False)])
        with tempfile.TemporaryDirectory() as d:
            code = overnight.run_plan('night', PLAN['steps'], Path(d) / 'q', runner=lambda c, l: next(codes),
                                      clock=Clock(), log=lambda m: None)
            summary = (Path(d) / 'q/summary.txt').read_text()
        self.assertEqual(code, 2)
        self.assertIn('STOPPED: step 2 exited 2', summary)


if __name__ == '__main__':
    unittest.main()
