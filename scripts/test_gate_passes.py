#!/usr/bin/env python3
"""CPU-only cache/landing proofs. Git repositories are disposable fixtures.

Cargo, renderer execution and GPU holds are mocked. No simulated pass is ever
written in the real repository's common directory.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import MagicMock, patch

import gate_passes as cache
import gpu_proofs_gate as proofs
import gpu_queue
import gpu_scope
import landing_gate as landing
import land_branch


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name).resolve()
        self.enterContext(patch.object(gpu_scope, 'learned_times_path',
                                       return_value=self.repo / '.git/gpu-test-times.json'))
        self.git('init', '-b', 'main')
        self.git('config', 'user.email', 'test@example.invalid')
        self.git('config', 'user.name', 'Cache test')
        self.write('Cargo.toml', '[workspace]\nmembers = ["crates/*"]\n')
        self.write('Cargo.lock', 'version = 4\n')
        self.write('.gitignore', 'target/\n.claude/orchestration/\ntests/fixtures/ignored.bin\n')
        self.write('scripts/ui-flows/manifest.json', '{"path_triggers": {}}')
        for name, deps in [('base', ''), ('a', '[dependencies]\nbase = {path="../base"}\n'),
                           ('b', ''), ('manifold-renderer', '[dependencies]\nbase = {path="../base"}\n')]:
            self.write(f'crates/{name}/Cargo.toml', f'[package]\nname = "{name}"\nversion = "0.1.0"\n{deps}')
            self.write(f'crates/{name}/src/lib.rs', 'pub fn original() {}\n')
        self.commit('base')
        self.git('branch', 'origin/main')
        self.git('checkout', '-b', 'work')
        self.write('crates/a/src/lib.rs', 'pub fn changed() {}\n')
        self.write('crates/b/src/lib.rs', 'pub fn changed() {}\n')
        self.write('crates/manifold-renderer/src/node_graph/primitives/invert.rs', 'pub fn invert() {}\n')
        self.commit('BUG-cache branch change')
        self.real_host_inputs = cache.host_inputs
        self.host = patch.object(cache, 'host_inputs', return_value={'host': 'cpu-test'})
        self.host.start()
        self.addCleanup(self.host.stop)
        self.output = io.StringIO()
        self.redirect = contextlib.redirect_stdout(self.output)
        self.redirect.__enter__()
        self.addCleanup(self.redirect.__exit__, None, None, None)

    def git(self, *args):
        out = subprocess.run(['git', '-C', str(self.repo), *args], capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        return out.stdout.strip()

    def write(self, name, text):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self, message):
        self.git('add', '--all')
        self.git('commit', '-m', message)

    def clippy(self, name):
        return cache.command_pass(self.repo, 'clippy/' + name,
                                  ['cargo', 'clippy', '-p', name, '--tests', '--', '-D', 'warnings'])

    def run_spec(self):
        return gpu_scope.plan_for_paths(
            ['crates/manifold-renderer/src/node_graph/primitives/invert.rs'], self.repo).runs()[0]

    def test_changed_crate_invalidates_only_its_dependency_closure(self):
        for name in ('a', 'b', 'manifold-renderer'):
            self.clippy(name).save(0)
        self.write('crates/a/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(self.clippy('a').record)
        self.assertIsNotNone(self.clippy('b').record)
        self.assertIsNotNone(self.clippy('manifold-renderer').record)
        self.write('crates/base/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(self.clippy('manifold-renderer').record)
        self.assertIsNotNone(self.clippy('b').record)

    def test_dependency_modes_aliases_and_target_specific_edges(self):
        self.write('crates/b/Cargo.toml', '[package]\nname="b"\nversion="0.1.0"\n'
                   '[target.\'cfg(unix)\'.build-dependencies]\nrenamed = {package="a", path="../a", optional=true}\n')
        self.assertEqual(cache.dependency_paths(self.repo, ['b']), ['crates/a', 'crates/b', 'crates/base'])

    def test_dirty_content_survives_commit_and_other_worktree(self):
        self.write('crates/a/src/lib.rs', 'pub fn uncommitted() {}\n')
        before = self.clippy('a')
        before.save(0)
        self.commit('same tested content')
        self.assertEqual(before.key, self.clippy('a').key)
        with tempfile.TemporaryDirectory() as directory:
            slot = Path(directory) / 'slot'
            self.git('worktree', 'add', '--detach', str(slot), 'HEAD')
            passed = cache.command_pass(slot, 'clippy/a', ['cargo', 'clippy', '-p', 'a', '--tests', '--', '-D', 'warnings'])
            self.assertEqual(before.path, passed.path)
            self.assertTrue(passed.reused())

    def test_failure_corruption_and_mid_run_edits_do_not_reuse(self):
        passed = self.clippy('a')
        passed.save(1)
        self.assertIsNone(self.clippy('a').record)
        passed.save(0)
        passed.save(1)
        self.assertIsNone(self.clippy('a').record)
        passed.save(0)
        passed.path.write_text('{broken')
        self.assertIsNone(self.clippy('a').record)
        self.write('crates/a/src/lib.rs', 'pub fn different() {}\n')
        passed.save(0)
        self.assertIsNone(self.clippy('a').record)

    def test_lock_toolchain_scripts_filters_features_and_fixtures_invalidate(self):
        proof = self.run_spec()
        for path in ('Cargo.lock', 'rust-toolchain.toml', 'scripts/gpu_scope.py',
                     'tests/fixtures/ignored.bin', 'crates/base/src/lib.rs'):
            with self.subTest(path=path):
                passed = cache.proof_pass(self.repo, proof)
                passed.save(0)
                self.assertIsNotNone(cache.proof_pass(self.repo, proof).record)
                self.write(path, 'changed ' + path)
                self.assertIsNone(cache.proof_pass(self.repo, proof).record)
        original = self.run_spec()
        cache.proof_pass(self.repo, original).save(0)
        changed = dict(original, filters=['different'])
        self.assertIsNone(cache.proof_pass(self.repo, changed).record)
        a = ['cargo', 'nextest', 'run', '-p', 'a', '-E', 'test(foo)']
        cache.command_pass(self.repo, 'tests', a).save(0)
        self.assertIsNone(cache.command_pass(self.repo, 'tests', a + ['--features', 'special']).record)

    def test_unrelated_main_merge_reuses_proof_and_merge_tree_matches(self):
        passed = cache.proof_pass(self.repo, self.run_spec())
        passed.save(0, 5)
        self.git('checkout', 'main')
        self.write('crates/b/src/independent.rs', 'pub fn main_change() {}\n')
        self.commit('unrelated main change')
        self.git('branch', '-f', 'origin/main', 'main')
        self.git('checkout', 'work')
        self.git('merge', 'origin/main', '--no-edit')
        self.assertTrue(cache.proof_pass(self.repo, self.run_spec()).reused())
        tree = self.git('rev-parse', 'HEAD^{tree}')
        self.git('checkout', 'main')
        self.git('merge', '--no-ff', 'work', '-m', 'landing')
        self.assertEqual(tree, self.git('rev-parse', 'HEAD^{tree}'))

    def test_queue_and_gate_share_exact_proof_key(self):
        run = self.run_spec()
        command = proofs.cargo_test_cmd(self.repo / 'Cargo.toml', run['targets'], lib=run['lib'])
        command += ['--', '--test-threads=1', *run['filters']]
        command += [a for skip in run['skips'] for a in ('--skip', skip)]
        before = cache.queued_proof(command, self.repo)
        self.assertIsNotNone(before)
        before.save(0, 2)
        after = cache.proof_pass(self.repo, run)
        self.assertEqual(before.key, after.key)
        self.assertTrue(after.reused())
        for extra in ('--ignored', '--exact', '--list'):
            self.assertIsNone(cache.queued_proof(command + [extra], self.repo))

    def test_actual_standalone_wrapper_and_queue_save_for_landing(self):
        run = self.run_spec()
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--path', 'crates/manifold-renderer/src/node_graph/primitives/invert.rs']
        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0), \
                patch.object(proofs, 'run_gate', return_value=(0, '')) as executed, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(executed.call_count, 1)
            self.assertTrue(cache.proof_pass(self.repo, run).reused())
        self.assertEqual(self.run_landing()[3], 0)
        # A changed source gets its pass from the queue entry point instead.
        self.write('crates/base/src/lib.rs', 'pub fn queue_checked() {}\n')
        self.commit('queue checked tree')
        command = proofs.cargo_test_cmd(self.repo / 'Cargo.toml', run['targets'], lib=run['lib'])
        command += ['--', '--test-threads=1', *run['filters']]
        command += [a for skip in run['skips'] for a in ('--skip', skip)]
        # Construct and save outside the Popen mock, since fingerprinting
        # deliberately uses real Git subprocesses in this temporary repository.
        passed = cache.queued_proof(command, self.repo)
        with patch.object(cache, 'queued_proof', return_value=passed), \
                patch.object(gpu_queue, '_run_build', return_value=0), \
                patch.object(gpu_queue, '_ancestor_holds', return_value=False), \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()), \
                patch.object(gpu_queue.subprocess, 'Popen') as process, \
                patch.object(passed, 'save') as saved:
            process.return_value.wait.return_value = 0
            self.assertEqual(gpu_queue.run_queued(command), 0)
            saved.assert_called_once()
            code, seconds = saved.call_args.args
        passed.save(code, seconds)
        self.assertTrue(cache.proof_pass(self.repo, run).reused())

    def test_moved_main_rechecks_only_changed_inputs_before_commit(self):
        passed = cache.proof_pass(self.repo, self.run_spec())
        passed.save(0, 2)
        tip = self.git('rev-parse', 'HEAD')
        self.git('checkout', 'main')
        self.write('crates/b/src/moved.rs', 'pub fn moved() {}\n')
        self.commit('main moved while branch was gated')
        slot = self.repo / 'target/slot'
        self.git('worktree', 'add', str(slot), 'work')

        def regate(*_):
            self.assertTrue(cache.proof_pass(slot, self.run_spec()).reused())
            return 0

        with patch.object(land_branch, 'MAIN', self.repo), \
                patch.object(land_branch, 'run_landing_gate', side_effect=regate) as gate:
            land_branch.merge_gated_tree('work', slot, 'landing', ['fake-gate'],
                                          self.repo / 'target/gate.log', tip)
        self.assertEqual(gate.call_count, 1)
        self.assertEqual(self.git('rev-parse', 'main^{tree}'), self.git('rev-parse', 'work^{tree}'))

    def test_unknown_dependency_runs_and_explains_why(self):
        self.write('crates/a/Cargo.toml', '[package]\nname="a"\n[dependencies]\nbad={path="../../../outside"}\n')
        self.assertIsNone(self.clippy('a').key)
        self.assertIn('external path dependency', self.output.getvalue())

    def test_raw_queue_long_run_does_not_certify_watchdog(self):
        passed = MagicMock()
        passed.reused.return_value = False
        with patch.object(cache, 'queued_proof', return_value=passed), \
                patch.object(gpu_queue, '_run_build', return_value=0), \
                patch.object(gpu_queue, '_ancestor_holds', return_value=False), \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()), \
                patch.object(gpu_queue.subprocess, 'Popen') as process, \
                patch.object(gpu_queue.time, 'monotonic', side_effect=[0, proofs.HANG_FLOOR_S + 1]):
            process.return_value.wait.return_value = 0
            self.assertEqual(gpu_queue.run_queued(['cargo', 'test']), 0)
        passed.save.assert_not_called()
        self.assertIn('cannot establish per-test hang allowances', self.output.getvalue())

    def test_worker_identity_is_not_rust_input_but_runtime_switch_is(self):
        with patch.dict(os.environ, {'CODEX_THREAD_ID': 'worker'}, clear=True), \
                patch.object(cache.platform, 'platform', return_value='test-platform'), \
                patch.object(cache.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'version', b'')):
            first = self.real_host_inputs(self.repo, True)
            os.environ['CODEX_THREAD_ID'] = 'lead'
            self.assertEqual(first, self.real_host_inputs(self.repo, True))
            os.environ['UPDATE_CONFORMANCE_GOLDENS'] = '1'
            self.assertNotEqual(first, self.real_host_inputs(self.repo, True))

    def test_budget_warning_reuses_pass_and_nightly_ignores_existing_pass(self):
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--path', 'crates/manifold-renderer/src/node_graph/primitives/invert.rs',
                '--budget', '360']

        def too_slow(manifest, filters, skips, targets, full, lib, timings, *rest):
            timings.append(('slow', 400, 'binary', 'ok'))
            return 0, ''

        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0), \
                patch.object(proofs, 'remember_times'), \
                patch.object(proofs, 'run_gate', side_effect=too_slow) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(run.call_count, 1)
        self.assertEqual(cache.proof_pass(self.repo, self.run_spec()).record['seconds'], 400)
        with patch.object(sys, 'argv', argv[:3] + ['--all']), \
                patch.object(proofs, 'build_tests', return_value=0) as build, \
                patch.object(proofs, 'run_gate', return_value=(0, '')) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(build.call_count, 1)
            self.assertEqual(run.call_count, 1)

    def run_landing(self, failed=None, keep_going=True):
        calls = []
        real_run = landing.run_cmd
        argv = ['landing_gate.py', '--repo', str(self.repo)] + ([] if keep_going else ['--fail-fast'])

        def run(command, cwd, timeout, live_log=None):
            if command[0] == 'git':
                return real_run(command, cwd, timeout, live_log)
            calls.append(command)
            if command[:2] == ['python3', 'scripts/gpu_proofs_gate.py']:
                with patch.object(sys, 'argv', command[1:] + ['--manifest-path', str(self.repo / 'Cargo.toml')]):
                    return proofs.main(), '', '', 0.01
            return (1 if failed and command[:2] == ['cargo', failed] else 0), '', '', 0.01

        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(sys, 'argv', argv))
            stack.enter_context(patch.object(landing, 'MAIN_CHECKOUT', self.repo))
            stack.enter_context(patch.object(landing, 'run_cmd', side_effect=run))
            stack.enter_context(patch.object(landing, 'reverse_deps', return_value=[]))
            stack.enter_context(patch('codex_checks.tooling_checks', return_value=[]))
            hold = stack.enter_context(patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()))
            stack.enter_context(patch.object(proofs, 'build_tests', return_value=0))
            proof_run = stack.enter_context(patch.object(proofs, 'run_gate', return_value=(0, '')))
            code = landing.main()
        return code, calls, hold.call_count, proof_run.call_count

    def test_second_full_gate_reuses_every_leg_without_build_or_gpu_hold(self):
        self.assertEqual(self.run_landing()[0], 0)
        started = time.monotonic()
        code, calls, holds, proofs_run = self.run_landing()
        elapsed = time.monotonic() - started
        self.assertEqual((code, calls, holds, proofs_run), (0, [], 0, 0))
        for label in ('design-status', 'deny', 'ignored-tests', 'clippy/a', 'clippy/b',
                      'flow-gate', 'tests/1', 'gpu-proofs', 'fresh-docs-index'):
            self.assertIn('[REUSED] ' + label, self.output.getvalue())
        print(f'unchanged simulated gate: {elapsed:.3f}s', file=sys.stderr)

    def test_landing_budget_warning_needs_no_named_red_or_repeat_gpu_hold(self):
        self.assertEqual(self.run_landing()[0], 0)
        cache.proof_pass(self.repo, self.run_spec()).save(0, 400)
        code, calls, holds, proofs_run = self.run_landing()
        self.assertEqual((code, calls, holds, proofs_run), (0, [], 0, 0))
        self.assertIn('GPU-PROOFS BUDGET: OVER (400s > 360s', self.output.getvalue())

    def test_failed_gate_retries_red_leg_but_reuses_earlier_green_legs(self):
        code, _, _, _ = self.run_landing(failed='deny', keep_going=False)
        self.assertEqual(code, 1)
        self.output.truncate(0)
        self.output.seek(0)
        code, calls, _, _ = self.run_landing(failed='deny', keep_going=False)
        self.assertEqual(code, 1)
        self.assertEqual(calls, [['cargo', 'deny', 'check', 'bans']])
        self.assertIn('[REUSED] design-status', self.output.getvalue())

    def test_only_a_gate_that_ran_every_check_exits_checks_red(self):
        self.assertEqual(self.run_landing(failed='deny')[0], landing.CHECKS_RED)
        self.assertEqual(self.run_landing(failed='deny', keep_going=False)[0], 1,
                         'a --fail-fast stop skipped later checks')
        self.write('scratch/untracked.txt', 'build output\n')
        code, calls, _, _ = self.run_landing(keep_going=True)
        self.assertEqual((code, calls), (1, []), 'a refused gate ran nothing')

    def test_named_red_lands_only_over_a_gate_that_ran_every_check(self):
        argv = ['land_branch.py', 'work', '--worktree', str(self.repo), '--message', 'landing',
                '--named-red', 'BUG-cache', '--reason', 'reviewed red']
        for gate_code, lands in [(1, False), (2, False), (landing.CHECKS_RED, True)]:
            with patch.object(sys, 'argv', argv), \
                    patch.object(land_branch, 'MAIN', self.repo), \
                    patch.object(land_branch, 'step', return_value=MagicMock(stdout='tip\n', returncode=0)), \
                    patch.object(land_branch, 'run_landing_gate', return_value=gate_code), \
                    patch.object(land_branch, 'merge_gated_tree') as merge, \
                    contextlib.redirect_stderr(io.StringIO()):
                if lands:
                    land_branch.main()
                else:
                    with self.assertRaises(SystemExit):
                        land_branch.main()
            self.assertEqual(merge.called, lands, f'gate exit {gate_code}')

    def test_gate_crate_edit_keeps_other_crate_and_gpu_passes(self):
        self.assertEqual(self.run_landing()[0], 0)
        self.write('crates/a/src/lib.rs', 'pub fn next_revision() {}\n')
        self.commit('only a changed')
        code, calls, _, proof_runs = self.run_landing()
        self.assertEqual(code, 0)
        self.assertEqual(proof_runs, 0)
        cargo = [c for c in calls if c[0] == 'cargo']
        self.assertTrue(cargo)
        self.assertTrue(all(c[c.index('-p') + 1] == 'a' for c in cargo), cargo)

    def test_changed_tree_during_gate_never_stamps_verdict(self):
        with patch.object(landing, 'GATED_HEAD') as expected, \
                patch.object(landing, 'MAIN_CHECKOUT', self.repo):
            expected.get.return_value = 'obsolete-commit'
            self.assertEqual(landing.finish(self.repo, 'origin/main', [('PASS', 'reused', 0, [])]), 1)
        self.assertFalse((self.repo / '.claude/orchestration/verdicts/BUG-cache.jsonl').exists())

    def test_reused_verdict_is_accepted_by_existing_merge_guard(self):
        # Use the real verdict writer and real guard without changing rules.
        source = Path(__file__).resolve().parent.parent
        for path in ('scripts/gate_runner.py', '.claude/hooks/agent-launch-guard.py'):
            self.write(path, (source / path).read_text())
        self.write('scripts/ui-flows/manifest.json', '{"path_triggers":{"crates/a/":["a-flow"]}}')
        self.commit('gate writer fixture')
        self.assertEqual(self.run_landing()[0], 0)
        self.assertEqual(self.run_landing()[1], [])
        verdict = json.loads((self.repo / '.claude/orchestration/verdicts/BUG-cache.jsonl').read_text().splitlines()[-1])
        self.assertTrue(verdict['pass'])
        spec = importlib.util.spec_from_file_location('merge_hook_cache_test', source / '.claude/hooks/preToolUseBash.py')
        hook = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(hook)
        with patch.object(hook, '_in_main_checkout', return_value=True), \
                patch.object(hook, '_current_branch', return_value='main'), \
                patch.object(hook, '_ORCH_VERDICTS_DIR', self.repo / '.claude/orchestration/verdicts'), \
                patch.object(hook, '_FLOW_MARKER_PATH', self.repo / '.claude/orchestration/flow-gate-marker.json'), \
                patch.object(hook, '_FLOW_MANIFEST_PATH', self.repo / 'scripts/ui-flows/manifest.json'):
            reason, context = hook.merge_verdict_guard('git merge --no-ff work', str(self.repo))
            flow_reason, flow_context = hook.flow_gate_guard('git merge --no-ff work', str(self.repo))
        self.assertIsNone(reason)
        self.assertIn('All BUG- tasks have passing verdicts', context)
        self.assertIsNone(flow_reason)
        self.assertIn('green marker matches', flow_context)


if __name__ == '__main__':
    unittest.main()
