#!/usr/bin/env python3
"""CPU-only cache/landing proofs. Git repositories are disposable fixtures.

Cargo, renderer execution and GPU holds are mocked. No simulated pass is ever
written in the real repository's common directory.
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
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


class FixtureQueries:
    """Memoize Git/metadata queries over byte-identical disposable fixtures.

    Production snapshots and every validation boundary still execute. Rescan
    bytes (not mtimes) and modes on every query, including ignored fixtures and
    shared refs/indexes, so mid-run edits and worktree changes remain observable.
    Only Git's immutable objects, reflogs, receipts and ignored gate output are
    omitted. This cache never escapes one test or handles a Git mutation.
    """
    def __init__(self, common, execute):
        self.common, self.execute = common, execute
        self.results = {}
        self.execute_run = subprocess.run
        self.metadata = {}

    @staticmethod
    def tree(root, excluded):
        digest = hashlib.sha256()
        def scan(directory, prefix=''):
            with os.scandir(directory) as entries:
                entries = sorted(entries, key=lambda entry: entry.name)
            for entry in entries:
                rel = prefix + entry.name
                if rel in excluded:
                    continue
                if entry.is_dir(follow_symlinks=False):
                    scan(entry.path, rel + '/')
                    continue
                mode = entry.stat(follow_symlinks=False).st_mode
                if entry.is_symlink():
                    content = os.readlink(entry.path).encode()
                else:
                    with open(entry.path, 'rb') as stream:
                        content = stream.read()
                digest.update(repr((rel, mode, len(content))).encode())
                digest.update(content)
        scan(root)
        return digest.digest()

    def __call__(self, repo, *args):
        if args[0] not in {'rev-parse', 'ls-tree', 'ls-files', 'diff', 'merge-base'}:
            raise AssertionError(f'fixture cache cannot run mutations: {args}')
        working_tree = (self.tree(repo, {'.git', 'target', '.claude/orchestration'})
                        if args[0] == 'diff' or '--others' in args else None)
        key = (str(repo), args, working_tree,
               self.tree(self.common, {'objects', 'logs', 'gate-passes-v1'}))
        if key not in self.results:
            self.results[key] = self.execute(repo, *args)
        return self.results[key]

    def run(self, command, *args, **kwargs):
        if command[:2] != ['cargo', 'metadata']:
            return self.execute_run(command, *args, **kwargs)
        repo = Path(command[command.index('--manifest-path') + 1]).parent
        key = (tuple(command),
               self.tree(repo, {'.git', 'target', '.claude/orchestration'}),
               tuple(sorted(kwargs.get('env', os.environ).items())))
        if key not in self.metadata:
            self.metadata[key] = self.execute_run(command, *args, **kwargs)
        return self.metadata[key]


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name).resolve()
        # This disposable workspace tests cache behavior, with its own smoke
        # selection; real repository proof ownership is covered by gpu_scope.
        self.enterContext(patch.object(gpu_scope, 'SMOKE_FILTERS', ['fixture_smoke::']))
        self.enterContext(patch.object(gpu_scope, 'learned_times_path',
                                       return_value=self.repo / '.git/gpu-test-times.json'))
        self.git('init', '-b', 'main')
        self.git('config', 'user.email', 'test@example.invalid')
        self.git('config', 'user.name', 'Cache test')
        self.write('Cargo.toml', '[workspace]\nmembers = ["crates/*"]\n')
        self.write('Cargo.lock', 'version = 4\n')
        self.write('deny.toml', '[bans]\ndeny = [\n'
                   '  { name = "manifold-nodes", wrappers = ["manifold-nodes"] },\n'
                   '  { name = "manifold-node-engine", wrappers = ["manifold-nodes"] },\n'
                   '  { name = "manifold-ui-paint", wrappers = ["manifold-ui-paint"] },\n'
                   ']\n')
        self.write('.gitignore', 'target/\n__pycache__/\n.claude/orchestration/\ntests/fixtures/ignored.bin\n')
        self.write('scripts/ui-flows/manifest.json', '{"flows": {}, "path_triggers": {}}')
        self.write('scripts/codex_regressions.json', '{}\n')
        packages = [('base', ''), ('a', '[dependencies]\nbase = {path="../base"}\n'),
                    ('b', ''), ('manifold-nodes', '[dependencies]\nbase = {path="../base"}\n'
                     'manifold-node-engine = {path="../manifold-node-engine"}\n'),
                    ('manifold-node-engine', '[dependencies]\nbase = {path="../base"}\n'),
                    ('manifold-ui-paint', '[dependencies]\na = {path="../a"}\n')]
        gpu_packages = {'manifold-nodes', 'manifold-node-engine', 'manifold-ui-paint'}
        for name, deps in packages:
            features = '[features]\ngpu-proofs = []\n' if name in gpu_packages else ''
            targets = ''
            if name == 'manifold-nodes':
                targets = ('\n[[test]]\nname = "gpu_proofs"\npath = "tests/gpu_proofs.rs"\n'
                           'required-features = ["gpu-proofs"]\n'
                           '\n[[test]]\nname = "glb_conformance"\npath = "tests/glb_conformance.rs"\n'
                           'required-features = ["gpu-proofs"]\n'
                           '\n[[test]]\nname = "uniform_layout_proof"\npath = "tests/uniform_layout_proof.rs"\n'
                           '\n[[test]]\nname = "uniform_layout_extended"\npath = "tests/uniform_layout_extended.rs"\n')
            self.write(f'crates/{name}/Cargo.toml',
                       f'[package]\nname = "{name}"\nversion = "0.1.0"\n{deps}{features}{targets}')
            self.write(f'crates/{name}/src/lib.rs', 'pub fn original() {}\n')
            if name == 'manifold-nodes':
                self.write(f'crates/{name}/tests/gpu_proofs.rs', '')
                self.write(f'crates/{name}/tests/glb_conformance.rs', '')
                self.write(f'crates/{name}/tests/uniform_layout_proof.rs', '')
                self.write(f'crates/{name}/tests/uniform_layout_extended.rs', '')
        self.commit('base')
        self.git('branch', 'origin/main')
        self.git('checkout', '-b', 'work')
        self.write('crates/a/src/lib.rs', 'pub fn changed() {}\n')
        self.write('crates/b/src/lib.rs', 'pub fn changed() {}\n')
        self.write('crates/manifold-nodes/src/registry.rs', 'pub fn invert() {}\n')
        self.commit('BUG-cache branch change')
        self.real_cache_git = cache.git
        self.fixture_git = FixtureQueries(self.repo / '.git', cache.git)
        self.enterContext(patch.object(cache, 'git', self.fixture_git))
        self.enterContext(patch.object(subprocess, 'run', self.fixture_git.run))
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

    def test_fixture_git_cache_observes_bytes_modes_refs_and_ignored_files(self):
        commands = [('ls-tree', '-r', '-z', 'HEAD'),
                    ('diff', '--name-only', '-z', 'HEAD'),
                    ('ls-files', '--others', '--exclude-standard', '-z'),
                    ('ls-files', '--others', '--ignored', '--exclude-standard', '-z', '--', 'tests'),
                    ('rev-parse', 'origin/main^{tree}')]

        def compare():
            for args in commands:
                self.assertEqual(cache.git(self.repo, *args), self.real_cache_git(self.repo, *args))

        with patch.object(self.fixture_git, 'execute', wraps=self.real_cache_git) as execute:
            compare()
            execute.reset_mock()
            compare()
            execute.assert_not_called()
            source = self.repo / 'crates/a/src/lib.rs'
            stat = source.stat()
            source.write_bytes(source.read_bytes().replace(b'changed', b'altered'))
            os.utime(source, ns=(stat.st_atime_ns, stat.st_mtime_ns))
            compare()
            self.assertTrue(execute.called, 'equal-sized edits with restored mtimes must invalidate')
            source.chmod(0o755)
            self.write('tests/fixtures/ignored.bin', 'ignored input')
            compare()
            self.commit('new fixture tree')
            self.git('branch', '-f', 'origin/main', 'HEAD')
            compare()
            source.unlink()
            compare()

    def run_spec(self):
        runs = self.scoped_runs()
        return next(run for run in runs if run['package'] == 'manifold-nodes')

    def scoped_runs(self):
        workspace = cache.Workspace(self.repo)
        plan = gpu_scope.plan_for_paths(
            ['crates/manifold-nodes/src/registry.rs'], self.repo,
            workspace=workspace)
        return proofs.normalize_runs(workspace, [dict(run, full=False) for run in plan.runs()])

    @staticmethod
    def call_packages(calls):
        # run_gate appends package, target and budgeted after the test lists.
        return sorted({call.args[-4] for call in calls})

    def test_bad_receipt_is_cache_miss_and_fresh_execution_passes(self):
        for unreadable in (False, True):
            with self.subTest(unreadable=unreadable), cache.session():
                original = self.clippy('a')
                original.directory.mkdir(parents=True, exist_ok=True)
                original.path.write_text('{broken json')
                real_read = Path.read_text

                def read(path, *args, **kwargs):
                    if unreadable and path == original.path:
                        raise PermissionError('receipt unreadable')
                    return real_read(path, *args, **kwargs)

                with patch.object(Path, 'read_text', read):
                    planned = self.clippy('a')
                self.assertEqual(planned.key, original.key)
                self.assertIsNone(planned.record)
                with patch.object(landing, 'run_cmd', return_value=(0, '', '', 0.01)) as run:
                    result = landing.run_check('clippy/a', ['cargo', 'clippy', '-p', 'a'],
                                               self.repo, 30, passed=planned)
                self.assertEqual(result[0], 0)
                run.assert_called_once()
                self.assertIsNotNone(self.clippy('a').record)

    def test_failed_receipt_write_still_validates_ignored_inputs_at_verdict(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        with cache.session(), patch.object(landing, 'MAIN_CHECKOUT', self.repo), \
                patch.object(landing, 'GATED_HEAD') as head:
            head.get.return_value = None
            landing.RAN_EVERY_CHECK.set(True)
            planned = self.clippy('a')
            with patch.object(cache.os, 'replace', side_effect=PermissionError('cache unavailable')), \
                    patch.object(landing, 'run_cmd', return_value=(0, '', '', 0.01)):
                result = landing.run_check('clippy/a', ['cargo', 'clippy', '-p', 'a'],
                                           self.repo, 30, passed=planned)
            self.assertEqual(result[0], 0)
            self.assertIn(planned, cache.accepted_passes())
            self.assertFalse(planned.path.exists())
            self.write('tests/fixtures/ignored.bin', 'after')
            self.assertEqual(landing.finish(self.repo, 'origin/main',
                                            [('PASS', 'clippy/a', 0, [])]), 1)
            self.assertFalse(landing.RAN_EVERY_CHECK.get())
            self.assertIn('inputs changed before verdict publication', self.output.getvalue())

    def test_proof_input_change_is_a_nonwaivable_gate_refusal(self):
        landing.RAN_EVERY_CHECK.set(True)
        with patch.object(landing, 'run_cmd', return_value=(
                proofs.INPUTS_CHANGED, 'GPU-PROOFS GATE: FAIL (inputs changed before receipt publication)', '', 0.01)):
            result = landing.run_check('gpu-proofs', ['python3', 'scripts/gpu_proofs_gate.py'],
                                       self.repo, 30)
        self.assertEqual(result[0], proofs.INPUTS_CHANGED)
        self.assertFalse(landing.RAN_EVERY_CHECK.get())

    def test_moving_main_during_design_check_keeps_pinned_diff_identity(self):
        original = cache.command_pass
        seen = []

        def moving_main(repo, label, cmd):
            passed = original(repo, label, cmd)
            if label == 'design-status':
                seen.append(cmd[-2])
                # Only the remote-tracking ref changes, not branch inputs.
                self.git('branch', '-f', 'origin/main', 'HEAD')
                self.assertTrue(passed.unchanged())
            return passed

        base = self.git('merge-base', 'origin/main', 'HEAD')
        with patch.object(cache, 'command_pass', side_effect=moving_main):
            self.assertEqual(self.run_landing()[0], 0)
        self.assertEqual(seen, [base])

    def test_pinned_flow_merge_base_ignores_remote_ref_movement(self):
        base = self.git('merge-base', 'origin/main', 'HEAD')
        cmd = ['python3', 'scripts/run_ui_flows.py', '--touched', f'{base}...HEAD']
        passed = cache.command_pass(self.repo, 'flow-gate', cmd)
        self.git('branch', '-f', 'origin/main', 'HEAD')
        self.assertIsNot(passed.save(0), False)
        self.assertTrue(cache.command_pass(self.repo, 'flow-gate', cmd).reused())

    def test_flow_receipt_reuses_across_metadata_only_main_merge(self):
        self.write('scripts/ui-flows/manifest.json',
                   '{"flows":{"a-flow":"scene"},'
                   '"path_triggers":{"crates/a/":["a-flow"]}}')
        self.write('scripts/ui-flows/a-flow.json', '[]')
        self.commit('flow fixture')
        base = self.git('merge-base', 'origin/main', 'HEAD')
        cmd = ['python3', 'scripts/run_ui_flows.py', '--touched', f'{base}...HEAD']
        passed = cache.command_pass(self.repo, 'flow-gate', cmd)
        passed.save(0)

        self.git('checkout', 'main')
        self.write('.beads/metadata.json', '{"updated":true}\n')
        self.commit('metadata only main merge')
        self.git('branch', '-f', 'origin/main', 'main')
        self.git('checkout', 'work')
        self.git('merge', 'origin/main', '--no-edit')

        merged_base = self.git('merge-base', 'origin/main', 'HEAD')
        merged = ['python3', 'scripts/run_ui_flows.py', '--touched',
                  f'{merged_base}...HEAD']
        self.assertTrue(cache.command_pass(self.repo, 'flow-gate', merged).reused())

    def test_flow_receipt_invalidates_selected_content_and_filter_selection(self):
        self.write('scripts/ui-flows/manifest.json',
                   '{"flows":{"a-flow":"scene"},'
                   '"path_triggers":{"crates/a/":["a-flow"]}}')
        self.write('scripts/ui-flows/a-flow.json', '[]')
        self.commit('flow fixture')
        base = self.git('merge-base', 'origin/main', 'HEAD')
        cmd = ['python3', 'scripts/run_ui_flows.py', '--touched', f'{base}...HEAD']

        cache.command_pass(self.repo, 'flow-gate', cmd).save(0)
        self.write('scripts/ui-flows/a-flow.json', '[{"action":"changed"}]')
        self.assertIsNone(cache.command_pass(self.repo, 'flow-gate', cmd).record)

        cache.command_pass(self.repo, 'flow-gate', cmd).save(0)
        self.write('crates/a/src/lib.rs', 'pub fn flow_source_changed() {}\n')
        self.assertIsNone(cache.command_pass(self.repo, 'flow-gate', cmd).record)

        cache.command_pass(self.repo, 'flow-gate', cmd).save(0)
        self.write('tests/fixtures/ignored.bin', 'flow fixture changed')
        self.assertIsNone(cache.command_pass(self.repo, 'flow-gate', cmd).record)

        cache.command_pass(self.repo, 'flow-gate', cmd).save(0)
        original_filters_key = cache.command_pass(self.repo, 'flow-gate', cmd).key
        # Same source and manifest, different selection: the empty range must
        # not reuse the a-flow receipt simply because file contents match.
        changed_filters = cache.command_pass(
            self.repo, 'flow-gate', [*cmd[:-1], 'HEAD...HEAD'])
        self.assertNotEqual(changed_filters.key, original_filters_key)
        self.assertIsNone(changed_filters.record)

    def test_malformed_flow_scope_disables_reuse(self):
        for scope in ('', 'origin/main..HEAD', 'origin/main...'):
            with self.subTest(scope=scope):
                passed = cache.command_pass(
                    self.repo, 'flow-gate',
                    ['python3', 'scripts/run_ui_flows.py', '--touched', scope])
                self.assertIsNone(passed.key)

    def test_policy_red_retains_timings_and_rechecks_without_execution(self):
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--package', 'manifold-nodes', '--test', 'gpu_proofs', '--budget', '1']
        table = self.repo / 'scripts/gpu_test_times.json'
        self.write('scripts/gpu_test_times.json', '{"tests": {}}')

        def measured(manifest, filters, skips, targets, full, lib, timings, *rest):
            timings.append(('slow', 80, 'gpu_proofs', 'ok'))
            return 0, ''

        run_spec = dict(package='manifold-nodes', targets=['gpu_proofs'], lib=False,
                        filters=[], skips=[], budgeted=True)
        with patch.object(sys, 'argv', argv), \
                patch.object(gpu_scope, 'TIMES_PATH', table), \
                patch.object(proofs, 'build_tests', return_value=0) as build, \
                patch.object(proofs, 'run_gate', side_effect=measured) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()) as hold:
            self.assertEqual(proofs.main(), 5)
            saved = cache.proof_pass(self.repo, run_spec)
            self.assertIsNotNone(saved.record)
            self.assertEqual(saved.record['timings'][0]['test'], 'slow')
            for mock in (build, run, hold):
                mock.reset_mock()
            self.assertEqual(proofs.main(), 5, 'reuse must not hide the policy red')
            self.write('scripts/gpu_test_times.json',
                       '{"tests": {"manifold-nodes/gpu_proofs/slow": 80}}')
            self.assertEqual(proofs.main(), 0)
            for mock in (build, run, hold):
                mock.assert_not_called()
            self.assertEqual(saved.key, cache.proof_pass(self.repo, run_spec).key)
            self.write('crates/manifold-nodes/src/lib.rs', 'pub fn changed_input() {}')
            self.assertIsNone(cache.proof_pass(self.repo, run_spec).record)

    def test_failed_run_keeps_earlier_pass_and_retries_only_failure(self):
        runs = self.scoped_runs()
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--path', 'crates/manifold-nodes/src/registry.rs']
        count = 0

        def execute(*args):
            nonlocal count
            count += 1
            if count == 2:
                self.assertIsNotNone(cache.proof_pass(self.repo, runs[0]).record,
                                     'first pass must be published before second run')
                return 1, 'test result: FAILED. 0 passed; 1 failed;\n'
            return 0, ''

        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0), \
                patch.object(proofs, 'run_gate', side_effect=execute) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertNotEqual(proofs.main(), 0)
            run.reset_mock()
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(run.call_args.args[-4], runs[1]['package'])

    def test_owned_filter_receipts_require_passing_test_evidence(self):
        name = 'alpha_contract::effects_preserve_transparency'
        run = dict(self.run_spec(), package='manifold-nodes', target='gpu_proofs',
                   targets=['gpu_proofs'], lib=False, filters=[name])
        passed = cache.proof_pass(self.repo, run)
        for timings in (None, [], [proofs.timing_entry(
                'manifold-nodes', 'gpu_proofs', 'unrelated', 0, 'ok', True)]):
            passed.save(0, timings=timings)
            self.assertIsNone(cache.proof_pass(self.repo, run).record)
        passed.save(0, timings=[proofs.timing_entry(
            'manifold-nodes', 'gpu_proofs', name, 0, 'ok', True)])
        self.assertIsNotNone(cache.proof_pass(self.repo, run).record)

    def test_landing_reused_proofs_still_enforce_timing_policy(self):
        self.assertEqual(self.run_landing()[0], 0)
        run = self.run_spec()
        passed = cache.proof_pass(self.repo, run)
        passed.save(0, 80)
        record = json.loads(passed.path.read_text())
        record['timings'] = [proofs.timing_entry(
            run['package'], run['target'], 'missing_allowance', 80, 'ok', True)]
        passed.path.write_text(json.dumps(record))
        code, calls, holds, executed = self.run_landing()
        self.assertNotEqual(code, 0)
        self.assertEqual((calls, holds, executed), ([], 0, 0))
        self.assertIsNotNone(cache.proof_pass(self.repo, run).record)
        transcripts = list((self.repo / 'target/landing-logs').glob('gpu-proofs-*.log'))
        self.assertTrue(any('GPU-PROOFS TIMING: FAIL' in log.read_text()
                            for log in transcripts))

    def test_failed_execution_with_changed_inputs_is_a_gate_refusal(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        with cache.session():
            planned = self.clippy('a')

            def fail(*args, **kwargs):
                self.write('tests/fixtures/ignored.bin', 'after')
                return 1, '', 'test failure', 0.01

            landing.RAN_EVERY_CHECK.set(True)
            with patch.object(landing, 'run_cmd', side_effect=fail):
                result = landing.run_check('clippy/a', ['cargo', 'clippy', '-p', 'a'],
                                           self.repo, 30, passed=planned)
            self.assertEqual(result[0], 1)
            self.assertFalse(landing.RAN_EVERY_CHECK.get())
            self.assertIn(planned, cache.accepted_passes())

    def test_publication_input_change_refuses_even_if_content_is_restored(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        real_dump = json.dump

        def mutate(*args, **kwargs):
            real_dump(*args, **kwargs)
            self.write('tests/fixtures/ignored.bin', 'after')

        with cache.session():
            planned = self.clippy('a')
            landing.RAN_EVERY_CHECK.set(True)
            with patch.object(cache.json, 'dump', side_effect=mutate), \
                    patch.object(landing, 'run_cmd', return_value=(0, '', '', 0.01)):
                result = landing.run_check('clippy/a', ['cargo', 'clippy', '-p', 'a'],
                                           self.repo, 30, passed=planned)
            self.assertEqual(result[0], 1)
            self.assertFalse(landing.RAN_EVERY_CHECK.get())
            self.assertFalse(planned.path.exists())
            self.write('tests/fixtures/ignored.bin', 'before')
            self.assertEqual(cache.changed_passes(cache.accepted_passes()), ['clippy/a'])

    def test_ignored_fixture_change_between_planning_and_reuse_refuses(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        self.clippy('a').save(0)
        with cache.snapshot():
            planned = self.clippy('a')
        self.assertIsNotNone(planned.record)
        self.write('tests/fixtures/ignored.bin', 'after')
        self.assertFalse(planned.reused())
        self.assertEqual(cache.changed_passes([planned]), ['clippy/a'])
        with patch.object(landing, 'run_cmd') as run:
            result = landing.run_check('clippy/a', ['cargo', 'clippy', '-p', 'a'],
                                       self.repo, 30, passed=planned)
        self.assertEqual(result[0], 1)
        run.assert_not_called()
        with patch.object(cache.os, 'replace') as publish:
            planned.save(0)
        publish.assert_not_called()

    def test_ignored_fixture_changed_by_build_refuses_before_gpu_admission(self):
        self.assertEqual(self.run_landing()[0], 0)
        self.write('crates/b/src/lib.rs', 'pub fn build_new_b() {}\n')
        self.commit('force one package build while other passes are cached')
        code, calls, holds, proofs_run = self.run_landing(
            on_build=lambda: self.write('tests/fixtures/ignored.bin', 'changed during build'))
        self.assertEqual((code, holds, proofs_run), (1, 0, 0))
        self.assertIn('inputs changed after build planning', self.output.getvalue())
        self.assertFalse(any(c[:3] == ['cargo', 'nextest', 'run'] and '--no-run' not in c
                             for c in calls))

    def test_fresh_publication_check_catches_late_ignored_fixture_change(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        planned = self.clippy('a')
        real_dump = json.dump

        def changed_during_write(*args, **kwargs):
            real_dump(*args, **kwargs)
            self.write('tests/fixtures/ignored.bin', 'after')

        with patch.object(cache.json, 'dump', side_effect=changed_during_write), \
                patch.object(cache.os, 'replace') as publish:
            planned.save(0)
        publish.assert_not_called()
        self.assertFalse(planned.path.exists())

    def test_host_tool_subprocesses_once_across_many_legs(self):
        with cache.session(), \
                patch.object(cache.platform, 'platform', return_value='test-platform'), \
                patch.object(cache.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'version', b'')) as run:
            for _ in range(20):
                self.real_host_inputs(self.repo, True)
            expected = 7 if sys.platform == 'darwin' else 5
            self.assertEqual(run.call_count, expected)
        with cache.session(), \
                patch.object(cache.platform, 'platform', return_value='test-platform'), \
                patch.object(cache.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'version', b'')) as run:
            self.real_host_inputs(self.repo, True)
            self.assertEqual(run.call_count, expected)

    def test_planning_shares_snapshot_but_validation_observes_new_content(self):
        self.write('tests/fixtures/ignored.bin', 'before')
        with patch.object(cache, 'selected_entries', wraps=cache.selected_entries) as selected:
            with cache.snapshot():
                passes = [self.clippy('a') for _ in range(10)]
            calls = selected.call_count
            self.assertLess(calls, 25)
            self.write('tests/fixtures/ignored.bin', 'after')
            self.assertEqual(len(cache.changed_passes(passes)), 10)
            self.assertEqual(selected.call_count, calls * 2)

    def test_tracked_root_scan_shared_only_within_each_boundary(self):
        self.write('tests/tracked.txt', 'root included in tooling scope')
        self.commit('track fixture root')
        with patch.object(cache, 'git', wraps=cache.git) as git:
            with cache.snapshot():
                passes = [cache.command_pass(self.repo, 'tool', ['python3', 'check.py'])
                          for _ in range(3)]
            scans = lambda: sum(call.args[1:] == ('ls-files', '-z')
                                for call in git.call_args_list)
            self.assertEqual(scans(), 1)
            self.assertEqual(cache.changed_passes(passes), [])
            self.assertEqual(scans(), 2)
            self.write('tests/fixtures/ignored.bin', 'changed after execution')
            self.assertEqual(len(cache.changed_passes(passes)), 3)
            self.assertEqual(scans(), 3)

    def test_changed_crate_invalidates_only_its_dependency_closure(self):
        for name in ('a', 'b', 'manifold-nodes'):
            self.clippy(name).save(0)
        self.write('crates/a/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(self.clippy('a').record)
        self.assertIsNotNone(self.clippy('b').record)
        self.assertIsNotNone(self.clippy('manifold-nodes').record)
        self.write('crates/base/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(self.clippy('manifold-nodes').record)
        self.assertIsNotNone(self.clippy('b').record)

    def test_dependency_modes_aliases_and_target_specific_edges(self):
        self.write('crates/b/Cargo.toml', '[package]\nname="b"\nversion="0.1.0"\n'
                   '[target.\'cfg(unix)\'.build-dependencies]\nrenamed = {package="a", path="../a", optional=true}\n')
        self.assertEqual(cache.dependency_paths(self.repo, ['b']), ['crates/a', 'crates/b', 'crates/base'])

    def test_metadata_added_and_removed_gpu_crate_updates_scope_and_cache(self):
        renderer_path = 'crates/manifold-nodes/src/registry.rs'
        initial = self.run_spec()
        cache.proof_pass(self.repo, initial).save(0)

        leaf_manifest = ('[package]\nname = "gpu-leaf"\nversion = "0.1.0"\n'
                         '[features]\ngpu-proofs = []\n')
        self.write('crates/gpu-leaf/Cargo.toml', leaf_manifest)
        self.write('crates/gpu-leaf/src/lib.rs', 'pub fn leaf() {}\n')
        self.write('crates/gpu-leaf/tests/gpu_proofs.rs', '#[test]\nfn leaf_gpu() {}\n')
        renderer_manifest = (self.repo / 'crates/manifold-nodes/Cargo.toml').read_text()
        self.write('crates/manifold-nodes/Cargo.toml',
                   renderer_manifest.replace(
                       'manifold-node-engine = {path="../manifold-node-engine"}\n',
                       'manifold-node-engine = {path="../manifold-node-engine"}\n'
                       'gpu-leaf = {path="../gpu-leaf"}\n', 1))

        workspace = cache.Workspace(self.repo)
        self.assertIn(('gpu-leaf', 'gpu-proofs'), workspace.feature_matrix())
        cpu = __import__('cpu_scope').plan_for_paths(
            ['crates/gpu-leaf/src/lib.rs'], self.repo, workspace=workspace)
        self.assertIn('gpu-leaf', cpu.packages)
        scope = gpu_scope.plan_for_paths([renderer_path], self.repo, workspace=workspace)
        self.assertIn('gpu-leaf', {run['package'] for run in scope.runs()})
        nightly = getattr(proofs, 'nightly_runs', proofs.all_runs)(workspace)
        self.assertIn('gpu-leaf', {run['package'] for run in nightly})
        self.assertIsNone(cache.proof_pass(self.repo, initial).record,
                          'adding a local dependency must invalidate the existing proof')

        refreshed = next(run for run in self.scoped_runs()
                         if run['package'] == 'manifold-nodes')
        cache.proof_pass(self.repo, refreshed).save(0)
        self.write('crates/gpu-leaf/src/lib.rs', 'pub fn revised_leaf() {}\n')
        self.assertIsNone(cache.proof_pass(self.repo, refreshed).record,
                          'a changed dependency must invalidate its dependent proof')

        renderer_manifest = (self.repo / 'crates/manifold-nodes/Cargo.toml').read_text()
        self.write('crates/manifold-nodes/Cargo.toml',
                   renderer_manifest.replace('gpu-leaf = {path="../gpu-leaf"}\n', ''))
        shutil.rmtree(self.repo / 'crates/gpu-leaf')
        workspace = cache.Workspace(self.repo)
        self.assertNotIn(('gpu-leaf', 'gpu-proofs'), workspace.feature_matrix())
        scope = gpu_scope.plan_for_paths([renderer_path], self.repo, workspace=workspace)
        self.assertNotIn('gpu-leaf', {run['package'] for run in scope.runs()})
        nightly = getattr(proofs, 'nightly_runs', proofs.all_runs)(workspace)
        self.assertNotIn('gpu-leaf', {run['package'] for run in nightly})
        restored = cache.proof_pass(self.repo, refreshed)
        self.assertEqual(restored.key, cache.proof_pass(self.repo, initial).key)
        self.assertIsNotNone(restored.record,
                             'removing the dependency must restore the original proof identity')

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
        for run in gpu_scope.plan_for_paths(
                ['crates/manifold-nodes/src/registry.rs'], self.repo).runs():
            with self.subTest(package=run.get('package', 'manifold-nodes')):
                command = proofs.cargo_test_cmd(
                    self.repo / 'Cargo.toml', run['targets'], lib=run['lib'],
                    package=run.get('package', 'manifold-nodes'))
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

    def test_proof_package_identity_and_dependency_closure(self):
        renderer = dict(self.run_spec(), targets=[], lib=True, filters=['shared::test'])
        paint = dict(renderer, package='manifold-ui-paint')
        renderer_pass = cache.proof_pass(self.repo, renderer)
        renderer_pass.save(0)
        paint_pass = cache.proof_pass(self.repo, paint)
        self.assertNotEqual(renderer_pass.key, paint_pass.key)
        self.assertIsNone(paint_pass.record)
        paint_pass.save(0)
        self.write('crates/manifold-ui-paint/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(cache.proof_pass(self.repo, paint).record)
        self.assertIsNotNone(cache.proof_pass(self.repo, renderer).record)
        cache.proof_pass(self.repo, paint).save(0)
        self.write('crates/a/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(cache.proof_pass(self.repo, paint).record)
        self.assertIsNotNone(cache.proof_pass(self.repo, renderer).record)
        cache.proof_pass(self.repo, paint).save(0)
        self.write('crates/manifold-foundation/assets/fonts/Inter-Regular.ttf', 'new font bytes')
        self.assertIsNone(cache.proof_pass(self.repo, paint).record)
        self.assertIsNone(cache.proof_pass(self.repo, renderer).record)
        cache.proof_pass(self.repo, paint).save(0)
        cache.proof_pass(self.repo, renderer).save(0)
        self.write('crates/base/src/lib.rs', 'pub fn newer() {}\n')
        self.assertIsNone(cache.proof_pass(self.repo, paint).record)
        self.assertIsNone(cache.proof_pass(self.repo, renderer).record)

    def test_ui_paint_queue_and_gate_share_exact_proof_key(self):
        run = dict(self.run_spec(), package='manifold-ui-paint', targets=[], lib=True,
                   filters=['clip_content_gpu::tests::gpu::'])
        command = proofs.cargo_test_cmd(self.repo / 'Cargo.toml', run['targets'],
                                        lib=run['lib'], package=run['package'])
        command += ['--', '--test-threads=1', *run['filters']]
        command += [a for skip in run['skips'] for a in ('--skip', skip)]
        queued = cache.queued_proof(command, self.repo)
        self.assertIsNotNone(queued)
        queued.save(0, 2)
        gated = cache.proof_pass(self.repo, run)
        self.assertEqual(queued.key, gated.key)
        self.assertTrue(gated.reused())
        for extra in ('--ignored', '--exact', '--list'):
            self.assertIsNone(cache.queued_proof(command + [extra], self.repo))

    def test_actual_standalone_wrapper_and_queue_save_for_landing(self):
        run = self.run_spec()
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--path', 'crates/manifold-nodes/src/registry.rs']
        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0), \
                patch.object(proofs, 'run_gate', return_value=(0, '')) as executed, \
            patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(executed.call_count, len(self.scoped_runs()))
            self.assertTrue(cache.proof_pass(self.repo, run).reused())
        self.assertEqual(self.run_landing()[3], 0)
        # A changed source gets its pass from the queue entry point instead.
        self.write('crates/base/src/lib.rs', 'pub fn queue_checked() {}\n')
        self.commit('queue checked tree')
        command = proofs.cargo_test_cmd(self.repo / 'Cargo.toml', run['targets'], lib=run['lib'],
                                        package=run['package'])
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
                patch.object(passed, 'unchanged', return_value=True), \
                patch.object(passed, 'save') as saved:
            process.return_value.wait.return_value = 0
            self.assertEqual(gpu_queue.run_queued(command), 0)
            saved.assert_called_once()
            code, seconds = saved.call_args.args
        passed.save(code, seconds)
        self.assertTrue(cache.proof_pass(self.repo, run).reused())

    def test_two_package_standalone_reuse_and_input_invalidation(self):
        path = 'crates/manifold-nodes/src/registry.rs'
        runs = self.scoped_runs()
        argv = ['gpu_proofs_gate.py', '--manifest-path', str(self.repo / 'Cargo.toml'),
                '--path', path]
        packages = sorted({run['package'] for run in runs})
        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0) as build, \
                patch.object(proofs, 'run_gate', return_value=(0, '')) as executed, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()) as hold:
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(self.call_packages(executed.call_args_list), packages)
            passes = [cache.proof_pass(self.repo, run) for run in runs]
            self.assertTrue(all(p.record for p in passes))
            self.assertEqual(len({p.path for p in passes}), len(passes))
            self.assertEqual(len(list(passes[0].directory.glob('*.json'))), len(passes))
            for mock in (build, executed, hold):
                mock.reset_mock()
            self.assertEqual(proofs.main(), 0)
            for mock in (build, executed, hold):
                mock.assert_not_called()
            code, calls, _, proof_runs = self.run_landing()
            self.assertEqual((code, proof_runs), (0, 0))
            self.assertFalse(any('scripts/gpu_proofs_gate.py' in cmd for cmd in calls))
            self.assertEqual(self.run_landing(), (0, [], 0, 0))

            # Renderer-only edits keep the engine pass. Engine edits also
            # invalidate renderer, which depends on the engine in production.
            renderer_only = ['manifold-nodes']
            engine_owners = {'manifold-node-engine',
                             *cache.Workspace(self.repo).reverse_dependencies(
                                 ['manifold-node-engine'])}
            engine_change = [package for package in packages if package in engine_owners]
            for changed, expected in [(path, renderer_only),
                                      ('crates/manifold-node-engine/src/lib.rs', engine_change)]:
                with self.subTest(changed=changed):
                    self.write(changed, 'pub fn revised() {}\n')
                    stale = sorted({run.get('package', 'manifold-nodes') for run in runs
                                    if not cache.proof_pass(self.repo, run).record})
                    self.assertEqual(stale, expected)
                    self.assertEqual(proofs.main(), 0)
                    self.assertEqual(self.call_packages(executed.call_args_list), expected)
                    built = build.call_args.args[1]
                    self.assertEqual(sorted({r.get('package', 'manifold-nodes') for r in built}), expected)
                    hold.assert_called_once()
                    for mock in (build, executed, hold):
                        mock.reset_mock()
                    self.assertEqual(proofs.main(), 0)
                    for mock in (build, executed, hold):
                        mock.assert_not_called()

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
        self.assertIn('cargo metadata failed', self.output.getvalue())

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

    def test_raw_ninety_second_proof_is_not_reusable_by_landing(self):
        run = self.run_spec()
        passed = cache.proof_pass(self.repo, run)
        with patch.object(cache, 'queued_proof', return_value=passed), \
                patch.object(passed, 'unchanged', return_value=True), \
                patch.object(gpu_queue, '_run_build', return_value=0), \
                patch.object(gpu_queue, '_ancestor_holds', return_value=False), \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()), \
                patch.object(gpu_queue.subprocess, 'Popen') as process, \
                patch.object(gpu_queue.time, 'monotonic', side_effect=[0, 90]):
            process.return_value.wait.return_value = 0
            self.assertEqual(gpu_queue.run_queued(['cargo', 'test']), 0)
        self.assertFalse(passed.path.exists())
        self.assertFalse(cache.proof_pass(self.repo, run).reused())
        self.assertGreater(self.run_landing()[3], 0)

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
                '--path', 'crates/manifold-nodes/src/registry.rs',
                '--budget', '360']

        def too_slow(manifest, filters, skips, targets, full, lib, timings, *rest):
            timings.append(('slow', 400, 'binary', 'ok'))
            return 0, ''

        with patch.object(sys, 'argv', argv), \
                patch.object(proofs, 'build_tests', return_value=0), \
                patch.object(proofs, 'remember_times'), \
                patch.object(proofs, 'unmeasured_heavy', return_value=[]), \
                patch.object(proofs, 'run_gate', side_effect=too_slow) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(run.call_count, len(self.scoped_runs()))
        self.assertEqual(cache.proof_pass(self.repo, self.run_spec()).record['seconds'], 400)
        with patch.object(sys, 'argv', argv[:3] + ['--all']), \
                patch.object(proofs, 'build_tests', return_value=0) as build, \
                patch.object(proofs, 'run_gate', return_value=(0, '')) as run, \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()):
            self.assertEqual(proofs.main(), 0)
            self.assertEqual(build.call_count, 1)
            nightly = proofs.all_runs(cache.Workspace(self.repo))
            self.assertEqual(run.call_count, len(nightly))
            self.assertEqual(self.call_packages(run.call_args_list),
                             [entry['package'] for entry in nightly])

    def run_landing(self, failed=None, keep_going=None, on_build=None):
        calls = []
        real_run = landing.run_cmd
        argv = ['landing_gate.py', '--repo', str(self.repo)]
        if keep_going is not None:
            argv.append('--keep-going' if keep_going else '--fail-fast')

        def run(command, cwd, timeout, live_log=None):
            if command[0] == 'git':
                return real_run(command, cwd, timeout, live_log)
            calls.append(command)
            if on_build and '--no-run' in command:
                on_build()
            if command[:3] == ['cargo', 'nextest', 'list']:
                package = command[command.index('-p') + 1]
                suites = {'fixture': {'binary-name': 'fixture', 'testcases': ['tests::fixture']}}
                if package == 'manifold-nodes':
                    suites.update({
                        'uniform_layout_proof': {'binary-name': 'uniform_layout_proof',
                                                 'testcases': ['uniform_layout_proof::fixture::test']},
                        'uniform_layout_extended': {'binary-name': 'uniform_layout_extended',
                                                    'testcases': ['uniform_layout_extended::fixture::test']},
                        'lib': {'binary-name': 'manifold_nodes',
                                'testcases': ['registry::fixture',
                                              'regenerates_in_sync']},
                    })
                return 0, json.dumps({'rust-suites': suites}), '', 0.01
            if command[:2] == ['python3', 'scripts/gpu_proofs_gate.py']:
                with patch.object(sys, 'argv', command[1:] + ['--manifest-path', str(self.repo / 'Cargo.toml')]):
                    return proofs.main(), '', '', 0.01
            return (1 if failed and command[:2] == ['cargo', failed] else 0), '', '', 0.01

        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(sys, 'argv', argv))
            stack.enter_context(patch.object(landing, 'MAIN_CHECKOUT', self.repo))
            stack.enter_context(patch.object(landing, 'run_cmd', side_effect=run))
            stack.enter_context(patch.object(landing, 'reverse_deps', return_value=[]))
            # The disposable fixture has no full nextest grouping table; the
            # ownership and pass invalidation assertions remain real.
            stack.enter_context(patch.object(cache.Workspace, 'validate_nextest', return_value=None))
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
                      'flow-gate', 'tests/a', 'tests/b', 'tests/manifold-nodes',
                      'gpu-proofs'):
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
        code, calls, _, proof_runs = self.run_landing(failed='deny')
        self.assertEqual(code, 1, 'the default stops at the expensive boundary')
        self.assertEqual(proof_runs, 0)
        self.assertFalse(any(command[1:2] == ['scripts/run_ui_flows.py']
                             and '--build-only' not in command for command in calls))
        code, calls, _, proof_runs = self.run_landing(failed='deny', keep_going=True)
        self.assertEqual(code, landing.CHECKS_RED)
        self.assertGreater(proof_runs, 0, 'the complete run must execute pending proofs')
        self.assertTrue(any(command[1:2] == ['scripts/run_ui_flows.py']
                            and '--build-only' not in command for command in calls))
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
                    patch.object(land_branch, 'run_landing_gate', return_value=gate_code) as gate, \
                    patch.object(land_branch, 'merge_gated_tree') as merge, \
                    contextlib.redirect_stderr(io.StringIO()):
                if lands:
                    land_branch.main()
                else:
                    with self.assertRaises(SystemExit):
                        land_branch.main()
            self.assertEqual(merge.called, lands, f'gate exit {gate_code}')
            self.assertIn('--keep-going', gate.call_args.args[0])

    def test_gate_crate_edit_keeps_other_crate_and_gpu_passes(self):
        self.assertEqual(self.run_landing()[0], 0)
        self.write('crates/a/src/lib.rs', 'pub fn next_revision() {}\n')
        self.commit('only a changed')
        expected_proofs = sum(not cache.proof_pass(self.repo, run).record
                              for run in self.scoped_runs())
        code, calls, _, proof_runs = self.run_landing()
        self.assertEqual(code, 0)
        self.assertEqual(proof_runs, expected_proofs)
        cargo = [c for c in calls if c[0] == 'cargo']
        self.assertTrue(cargo)
        expected_packages = {'a', *cache.Workspace(self.repo).reverse_dependencies(['a'])}
        package_args = {c[c.index('-p') + 1] for c in cargo if '-p' in c}
        self.assertEqual(package_args, expected_packages)

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
            (self.repo / path).chmod((source / path).stat().st_mode & 0o777)
        self.write('scripts/ui-flows/manifest.json', '{"flows":{"a-flow":"scene"},"path_triggers":{"crates/a/":["a-flow"]}}')
        self.write('scripts/ui-flows/a-flow.json', '[]')
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
