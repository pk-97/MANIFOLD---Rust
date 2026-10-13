#!/usr/bin/env python3
"""land_branch.py with git and the gate faked: the landing lock, the
named-red carry-over after a moved main, and the landing-runs.jsonl row.
Nothing here touches a real repository."""
import contextlib
import fcntl
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import land_branch
from gate_cancellation import Cancelled
from landing_gate import CHECKS_RED


def gate_text(*reds, wait=None):
    lines = ['        FAIL [   1.0s] crate nextest_line_is_not_a_label']
    lines += [f'FAIL {label} (3s)' for label in reds]
    if wait is not None:
        lines.append(f'gpu-queue wait: {wait}s')
    return '\n'.join(lines) + '\n'


class FakeGit:
    """Answers land_branch.step by step name; origin/main moves on demand."""

    def __init__(self, lock_dir, origins, conflict=False, push_fails=False):
        self.lock_dir, self.origins = lock_dir, list(origins)
        self.conflict, self.push_fails = conflict, push_fails
        self.calls, self.lock_held_at = [], {}

    def __call__(self, name, cmd, cwd, check=True, cleanup=False):
        self.calls.append(name)
        out, code = 'tip\n', 0
        if name == 'origin/main':
            out = self.origins.pop(0) if len(self.origins) > 1 else self.origins[0]
        elif name == 'local main':
            out = self.origins[0]
        elif name == 'git common dir':
            out = str(self.lock_dir)
        elif name in ('gated tree', 'verify merge tree'):
            out = 'tree'
        elif name == 'merge --no-ff to main':
            code = int(self.conflict)
        elif name == 'push main':
            code = int(self.push_fails)
        if name in ('merge --no-ff to main', 'push main', 'commit beads'):
            self.lock_held_at[name] = lock_is_held(self.lock_dir / land_branch.LOCK_NAME)
        if code and check and not cleanup:
            sys.exit(1)
        return subprocess.CompletedProcess(cmd, code, out, '')


def lock_is_held(path):
    with open(path, 'a+') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(handle, fcntl.LOCK_UN)
        return False


class LockTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / land_branch.LOCK_NAME

    def test_lock_is_exclusive_and_waiter_names_holder(self):
        out = io.StringIO()
        order = []
        with land_branch.landing_lock(self.path, 'first-branch (pid 1)'):
            self.assertTrue(lock_is_held(self.path))

            def second():
                with land_branch.landing_lock(self.path, 'second') as waited:
                    order.append(('second', waited))

            with contextlib.redirect_stdout(out):
                waiter = threading.Thread(target=second)
                waiter.start()
                time.sleep(0.3)
                order.append(('first releases', None))
        waiter.join(5)
        self.assertEqual([o[0] for o in order], ['first releases', 'second'])
        self.assertGreater(order[1][1], 0.2)
        self.assertIn('held by first-branch (pid 1)', out.getvalue())
        self.assertFalse(lock_is_held(self.path))

    def test_lock_released_on_failure_and_on_cancel(self):
        for error in (SystemExit(1), RuntimeError('boom'), Cancelled(2)):
            with self.subTest(error=type(error).__name__):
                with self.assertRaises(BaseException):
                    with land_branch.landing_lock(self.path, 'x'):
                        raise error
                self.assertFalse(lock_is_held(self.path))


class CarryOverTests(unittest.TestCase):
    def test_only_identical_red_set_over_a_complete_gate(self):
        named = frozenset({'clippy/a', 'flow-gate'})
        allowed = land_branch.carry_over_allowed
        self.assertTrue(allowed(named, CHECKS_RED, frozenset({'clippy/a', 'flow-gate'})))
        self.assertFalse(allowed(named, CHECKS_RED, named | {'tests/1'}), 'a new red')
        self.assertFalse(allowed(named, CHECKS_RED, frozenset({'flow-gate'})), 'a changed set')
        self.assertFalse(allowed(named, 1, named), 'gate did not run every check')
        self.assertFalse(allowed(frozenset(), CHECKS_RED, frozenset()), 'no named red')

    def test_labels_and_gpu_wait_parse_from_the_gate_summary(self):
        text = gate_text('clippy/a', 'flow-gate', wait=12) + gate_text(wait=3)
        self.assertEqual(land_branch.red_labels(text), {'clippy/a', 'flow-gate'})
        self.assertEqual(land_branch.gpu_wait_seconds(text), 15)
        self.assertIsNone(land_branch.gpu_wait_seconds('PASS tests (3s)\n'))


class FlowTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.wt = self.root / 'slot'
        self.wt.mkdir()

    def land(self, fake, gates, named_red=False, close=False):
        """Runs main() with fakes; gates is [(exit code, gate text)] per run."""
        queue = list(gates)

        def gate(cmd, cwd, log_path):
            code, text = queue.pop(0)
            log_path.write_text(text)
            if isinstance(code, BaseException):
                raise code
            return code

        argv = ['land_branch.py', 'lane/x', '--worktree', str(self.wt), '--message', 'm']
        if named_red:
            argv += ['--named-red', 'BUG-red', '--reason', 'reviewed']
        if close:
            argv += ['--close-bead', 'BUG-done']
        code = None
        with patch.object(sys, 'argv', argv), \
                patch.object(land_branch, 'start_checks', return_value=False), \
                patch.object(land_branch, 'MAIN', self.root), \
                patch.object(land_branch, 'step', side_effect=fake), \
                patch.object(land_branch, 'run_landing_gate', side_effect=gate), \
                patch.object(land_branch, 'record_incomplete', return_value=130), \
                contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            try:
                code = land_branch.main()
            except SystemExit as stop:
                code = ('exit', stop.code)
        self.assertEqual(queue, [], 'every scripted gate run happened')
        rows = (self.root / land_branch.RUNS_LOG).read_text().splitlines()
        return code, json.loads(rows[-1])

    def assert_unlocked(self):
        self.assertFalse(lock_is_held(self.root / land_branch.LOCK_NAME))

    def test_green_landing_one_push_under_lock(self):
        fake = FakeGit(self.root, ['base'])
        code, row = self.land(fake, [(0, gate_text(wait=7))], close=True)
        self.assertIsNone(code)
        self.assertEqual(fake.calls.count('push main'), 1)
        self.assertNotIn('push beads', fake.calls)
        self.assertEqual(fake.lock_held_at, {'merge --no-ff to main': True, 'commit beads': True,
                                             'push main': True})
        self.assertLess(fake.calls.index('commit beads'), fake.calls.index('push main'))
        self.assert_unlocked()
        self.assertEqual({k: row[k] for k in ('branch', 'outcome', 'gate_runs', 'moved_main_regates',
                                              'gpu_wait_s')},
                         {'branch': 'lane/x', 'outcome': 'landed', 'gate_runs': 1,
                          'moved_main_regates': 0, 'gpu_wait_s': 7})
        self.assertIsNotNone(row['end'])
        self.assertGreaterEqual(row['lock_wait_s'], 0)

    def test_moved_main_regates_outside_the_lock(self):
        fake = FakeGit(self.root, ['base', 'moved', 'moved'])
        code, row = self.land(fake, [(0, gate_text()), (0, gate_text())])
        self.assertIsNone(code)
        self.assertEqual((row['gate_runs'], row['moved_main_regates'], row['outcome']), (2, 1, 'landed'))
        self.assertEqual(fake.calls.count('push main'), 1)
        merge_in = fake.calls.index('merge moved main into branch')
        self.assertLess(merge_in, fake.calls.index('merge --no-ff to main'))

    def test_gate_runs_never_hold_the_lock(self):
        fake = FakeGit(self.root, ['base', 'moved', 'moved'])
        held = []
        real_gate = land_branch.gate

        def gate(*args):
            held.append(lock_is_held(self.root / land_branch.LOCK_NAME))
            return real_gate(*args)

        with patch.object(land_branch, 'gate', side_effect=gate):
            self.land(fake, [(0, gate_text()), (0, gate_text())])
        self.assertEqual(held, [False, False])

    def test_named_red_carries_over_only_for_identical_reds(self):
        cases = [
            (gate_text('clippy/a'), CHECKS_RED, True),
            (gate_text('clippy/a', 'tests/1'), CHECKS_RED, False),
            (gate_text(), CHECKS_RED, False),
            (gate_text('clippy/a'), 1, False),
        ]
        for regate_text, regate_code, lands in cases:
            with self.subTest(regate=regate_text, code=regate_code):
                fake = FakeGit(self.root, ['base', 'moved', 'moved'])
                code, row = self.land(fake, [(CHECKS_RED, gate_text('clippy/a')),
                                             (regate_code, regate_text)], named_red=True)
                self.assertEqual(row['outcome'], 'landed' if lands else 'red')
                self.assertEqual('push main' in fake.calls, lands)
                self.assertEqual('no-gate verdict' in fake.calls, lands)
                self.assert_unlocked()

    def test_moved_main_green_drops_the_verdict(self):
        fake = FakeGit(self.root, ['base', 'moved', 'moved'])
        code, row = self.land(fake, [(CHECKS_RED, gate_text('clippy/a')), (0, gate_text())],
                              named_red=True)
        self.assertEqual(row['outcome'], 'landed')
        self.assertNotIn('no-gate verdict', fake.calls)

    def test_outcome_rows(self):
        cases = [
            ('red', FakeGit(self.root, ['base']), [(1, gate_text('clippy/a'))]),
            ('conflict', FakeGit(self.root, ['base'], conflict=True), [(0, gate_text())]),
            ('error', FakeGit(self.root, ['base'], push_fails=True), [(0, gate_text())]),
            ('cancelled', FakeGit(self.root, ['base']), [(Cancelled(2), gate_text())]),
        ]
        for outcome, fake, gates in cases:
            with self.subTest(outcome=outcome):
                code, row = self.land(fake, gates)
                self.assertEqual(row['outcome'], outcome)
                self.assertEqual(code, 130 if outcome == 'cancelled' else ('exit', 1))
                self.assert_unlocked()


def git(cwd, *args):
    return subprocess.run(['git', *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout.strip()


class StartCheckTests(unittest.TestCase):
    """A real throwaway repo: the slot must hold the named branch's tip, clean
    and quiet, before land_branch fetches or gates anything."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        git(self.repo, 'init', '-q', '-b', 'main')
        git(self.repo, 'config', 'user.name', 'Test')
        git(self.repo, 'config', 'user.email', 'test@example.invalid')
        git(self.repo, '-c', 'commit.gpgsign=false', 'commit', '-q', '--allow-empty', '-m', 'base')
        git(self.repo, 'update-ref', 'refs/remotes/origin/main', 'HEAD')
        self.wt = self.root / 'slot'
        git(self.repo, 'worktree', 'add', '-q', '-b', 'lane/x', str(self.wt))
        (self.wt / 'work.txt').write_text('work\n')
        git(self.wt, 'add', '--', 'work.txt')
        git(self.wt, '-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'work', '--', 'work.txt')
        self.age_files()
        self.later = time.time()
        self.busy_patch = patch.object(land_branch, 'codex_busy', return_value=None)
        self.busy = self.enterContext(self.busy_patch)

    def age_files(self):
        old = time.time() - 3600
        for path in self.wt.rglob('*'):
            if '.git' not in path.parts:
                os.utime(path, (old, old), follow_symlinks=False)

    def land(self, *extra, branch='lane/x'):
        """main() with the gate and git steps faked; returns (exit, rows, stderr, fakes)."""
        argv = ['land_branch.py', branch, '--worktree', str(self.wt), '--message', 'm', *extra]
        err = io.StringIO()
        with patch.object(sys, 'argv', argv), \
                patch.object(land_branch, 'MAIN', self.root), \
                patch.object(land_branch, 'step') as step, \
                patch.object(land_branch, 'run_landing_gate', return_value=1) as gate, \
                contextlib.redirect_stdout(io.StringIO()) as out, \
                contextlib.redirect_stderr(err):
            step.return_value = subprocess.CompletedProcess([], 0, 'tip\n', '')
            try:
                code = land_branch.main()
            except SystemExit as stop:
                code = stop.code
        log = self.root / land_branch.RUNS_LOG
        rows = [json.loads(r) for r in log.read_text().splitlines()] if log.exists() else []
        return code, rows, out.getvalue() + err.getvalue(), step, gate

    def assert_refused(self, why):
        code, rows, text, step, gate = self.land()
        self.assertEqual(code, 1)
        self.assertIn('REFUSED', text)
        self.assertIn(why, text)
        self.assertTrue('Check:' in text or 'rerun' in text, 'names the check to rerun')
        self.assertFalse(step.called, 'refused before any fetch')
        self.assertFalse(gate.called, 'refused before any gate')
        self.assertEqual([r['outcome'] for r in rows], ['refused'])

    def test_help_and_bad_args_write_no_row(self):
        for argv in (['--help'], ['--worktree', str(self.wt)]):
            with self.subTest(argv=argv), patch.object(sys, 'argv', ['land_branch.py', *argv]), \
                    patch.object(land_branch, 'MAIN', self.root), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    land_branch.main()
            self.assertFalse((self.root / land_branch.RUNS_LOG).exists())

    def test_clean_quiet_tip_passes_start_checks(self):
        self.assertFalse(land_branch.start_checks('lane/x', self.wt))

    def test_refuses_missing_branch(self):
        git(self.wt, 'switch', '-q', '--detach')
        git(self.repo, 'branch', '-q', '-D', 'lane/x')
        self.assert_refused('does not exist')

    def test_refuses_slot_holding_other_work(self):
        """The 2026-10-13 incident: the slot was reused, detached elsewhere."""
        git(self.wt, 'switch', '-q', '--detach', 'HEAD~1')
        git(self.repo, 'branch', '-q', '-f', 'lane/x', 'main')
        git(self.wt, '-c', 'commit.gpgsign=false', 'commit', '-q', '--allow-empty', '-m', 'other')
        self.assert_refused('is not the tip')

    def test_refuses_dirty_tree(self):
        (self.wt / 'work.txt').write_text('changed\n')
        self.age_files()
        self.assert_refused('not clean')

    def test_refuses_busy_codex_job(self):
        self.busy.return_value = 'job j1 is running'
        self.assert_refused('job j1 is running')

    def test_refuses_recent_write_outside_target(self):
        (self.wt / 'work.txt').touch()
        self.assert_refused('modified in the last')

    def test_writes_under_target_and_git_do_not_count(self):
        (self.wt / 'target').mkdir()
        (self.wt / 'target' / 'build.log').write_text('x\n')
        self.assertIsNone(land_branch.recent_write(self.wt, time.time()))

    def test_codex_busy_reuses_broker_state(self):
        real = self.busy_patch.temp_original
        state = self.root / 'state'
        with patch('codex_brokers.state_dirs', return_value=[state]) as dirs, \
                patch('codex_brokers.busy_reason', return_value='job j2 is queued') as reason:
            self.assertEqual(real(self.wt), 'job j2 is queued')
        dirs.assert_called_once_with(self.wt)
        reason.assert_called_once_with(state)

    def fresh_branch(self):
        """A lane branch cut from main with no commits of its own."""
        git(self.wt, 'switch', '-q', '-c', 'lane/fresh', 'main')
        self.age_files()

    def test_uncommitted_work_on_a_fresh_branch_refuses_as_dirty(self):
        self.fresh_branch()
        (self.wt / 'forgot.txt').write_text('uncommitted\n')
        self.age_files()
        code, rows, text, step, gate = self.land(branch='lane/fresh')
        self.assertEqual(code, 1)
        self.assertIn('not clean', text)
        self.assertEqual([r['outcome'] for r in rows], ['refused'])
        self.assertFalse(step.called or gate.called)

    def test_fresh_clean_branch_refuses_nothing_to_land(self):
        self.fresh_branch()
        code, rows, text, step, gate = self.land(branch='lane/fresh')
        self.assertEqual(code, 1)
        self.assertIn('nothing to land', text)
        self.assertNotIn('already landed', text)
        self.assertEqual([r['outcome'] for r in rows], ['refused'])
        self.assertFalse(step.called or gate.called)

    def test_start_check_exception_records_error_and_refuses(self):
        self.busy.side_effect = RuntimeError('broker state unreadable')
        code, rows, text, step, gate = self.land()
        self.assertEqual(code, 1)
        self.assertIn('REFUSED', text)
        self.assertIn('broker state unreadable', text)
        self.assertEqual([r['outcome'] for r in rows], ['error'])
        self.assertFalse(step.called or gate.called)

    def test_recent_pyc_write_does_not_refuse(self):
        cache = self.wt / 'scripts' / '__pycache__'
        cache.mkdir(parents=True)
        (cache / 'x.cpython-312.pyc').write_bytes(b'x')
        (self.wt / 'loose.pyc').write_bytes(b'x')
        self.assertIsNone(land_branch.recent_write(self.wt, time.time()))

    def test_already_landed_exits_zero_without_a_row(self):
        git(self.repo, '-c', 'commit.gpgsign=false', 'merge', '-q', '--no-ff', '-m', 'land', 'lane/x')
        git(self.repo, 'update-ref', 'refs/remotes/origin/main', 'main')
        code, rows, text, step, gate = self.land()
        self.assertEqual(code, 0)
        self.assertIn('already landed', text)
        self.assertEqual(rows, [])
        self.assertFalse(step.called or gate.called)

    def test_good_slot_reaches_the_gate(self):
        code, rows, _, step, gate = self.land()
        self.assertTrue(gate.called)
        self.assertEqual([r['outcome'] for r in rows], ['red'])


if __name__ == '__main__':
    unittest.main()
