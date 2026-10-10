#!/usr/bin/env python3
import tempfile
import unittest
import subprocess
from pathlib import Path
import sys
from unittest.mock import Mock, patch
sys.path.insert(0, str(Path(__file__).parent))

import codex_checks
import cpu_scope
import run_ui_flows
import gpu_scope
from gate_workspace import Workspace


class PlannerTests(unittest.TestCase):
    def test_worktree_safety_checks_are_selected(self):
        repo = Path(__file__).resolve().parents[1]
        for path in ("scripts/agent-worktree.py", "scripts/codex_brokers.py", "scripts/test_agent_worktree.py"):
            checks = codex_checks.tooling_checks(repo, [path])
            self.assertEqual([c["name"] for c in checks],
                             ["scripts/test_agent_worktree.py", "scripts/test_dev.py"])

    def test_feature_coverage_is_checked_without_builds(self):
        repo = Path(__file__).resolve().parents[1]
        for path in ("Cargo.toml", "crates/manifold-nodes/Cargo.toml", "scripts/feature_matrix.py"):
            checks = codex_checks.tooling_checks(repo, [path])
            coverage = [c for c in checks if c["name"] == "feature-coverage"]
            self.assertEqual(len(coverage), 1)
            self.assertEqual(coverage[0]["argv"],
                             ["python3", "-B", str(repo / "scripts/feature_matrix.py"), "--check-coverage"])
        self.assertNotIn("feature-coverage", [c["name"] for c in
                         codex_checks.tooling_checks(repo, ["docs/README.md"])])

    def test_rt_gate_configuration_selects_synthetic_checks(self):
        repo = Path(__file__).resolve().parents[1]
        for path in ("scripts/rt_noise_gate.py", "scripts/test_rt_noise_gate.py",
                     "scripts/rt_noise_baseline.json", "scripts/trunk_health.py"):
            checks = codex_checks.tooling_checks(repo, [path])
            expected = ["scripts/test_rt_noise_gate.py"]
            if path == "scripts/trunk_health.py":
                expected.append("scripts/test_landing_gate.py")
            if path.endswith(".py"):
                expected.append("scripts/test_dev.py")  # every script is inventoried
            if path == 'scripts/trunk_health.py':
                expected.append('scripts/test_trunk_health.py')
                expected.append('scripts/test_gpu_queue.py')
            self.assertEqual([c["name"] for c in checks], expected)
            self.assertEqual(checks[0]["argv"],
                             ["python3", "-B", str(repo / "scripts/test_rt_noise_gate.py")])

    def test_ui_trigger_matching_and_flow_file(self):
        manifest = {"path_triggers": {"crates/manifold-ui/src/panels/rt_quality_panel.rs": ["rt-quality"]}}
        filters, hits = run_ui_flows.filters_for_paths(
            ["crates/manifold-ui/src/panels/rt_quality_panel.rs", "scripts/ui-flows/foo.json"], manifest)
        self.assertEqual(filters, ["foo", "rt-quality"])
        self.assertEqual(hits["scripts/ui-flows/foo.json"], ["foo"])

    def test_gpu_plan_passes_paths_and_scopes_glb_only_for_gltf(self):
        repo = Path(__file__).resolve().parents[1]
        rt = "crates/manifold-gpu/src/metal/raytrace.rs"
        glb = "crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs"
        fixture = "tests/fixtures/gltf/khronos/manifest.json"
        for paths, glb_expected in [([rt], False), ([glb], True), ([fixture], True), ([rt, glb], True)]:
            with self.subTest(paths=paths):
                plan = codex_checks.build_plan(repo, paths)
                cmd = next(c["argv"] for c in plan["checks"] if c["name"] == "gpu-proofs")
                self.assertEqual([cmd[i + 1] for i, a in enumerate(cmd) if a == "--path"], sorted(paths))
                self.assertNotIn("--all", cmd)
                self.assertNotIn("--full-suite", cmd)
                self.assertEqual(plan["gpu_scope"]["glb"], glb_expected)
        plan = codex_checks.build_plan(repo, ["docs/README.md"])
        self.assertFalse(any(c["name"] == "gpu-proofs" for c in plan["checks"]))

    def test_plan_has_exact_tool_commands(self):
        with tempfile.TemporaryDirectory() as d:
            repo = Path(d)
            (repo / "scripts/ui-flows").mkdir(parents=True)
            (repo / "scripts/ui-flows/manifest.json").write_text('{"path_triggers": {}}')
            workspace = Workspace(repo, {'workspace_members': ['fixture'], 'packages': [{
                'id': 'fixture', 'name': 'fixture', 'manifest_path': str(repo / 'crates/fixture/Cargo.toml'),
                'targets': [], 'features': {}, 'dependencies': []}]})
            with patch("codex_regressions.inventory", return_value=[]), \
                    patch.object(codex_checks, 'Workspace', return_value=workspace):
                plan = codex_checks.build_plan(repo, ["scripts/run_ui_flows.py"])
            self.assertEqual(plan["checks"][0]["argv"], ["python3", "-B", str(repo.resolve() / "scripts/test_codex_checks.py")])

    def _mocked_plan(self, cpu_plan, gpu_plan=None):
        repo = Path(__file__).resolve().parents[1]
        if gpu_plan is None:
            gpu_plan = gpu_scope.Plan()
        workspace = Mock()
        workspace.owner.return_value = 'fixture'
        with patch.object(codex_checks, 'Workspace', return_value=workspace), \
                patch.object(cpu_scope, 'plan_for_paths', return_value=cpu_plan), \
                patch.object(gpu_scope, 'plan_for_paths', return_value=gpu_plan), \
                patch('codex_regressions.inventory', return_value=[]):
            return codex_checks.build_plan(repo, ['crates/fixture/src/lib.rs'])

    def test_private_folded_selection_is_forwarded_to_nextest(self):
        filterset = '(package(=fixture) & binary(=catalog) & test(/^private::folded::/))'
        plan = self._mocked_plan(cpu_scope.Plan(packages={'fixture'}, filters={filterset}))
        checks = plan['checks']
        build = next(c for c in checks if c['name'] == 'tests-build/fixture')
        run = next(c for c in checks if c['name'] == 'tests/fixture')
        self.assertEqual(build['argv'][-1], filterset)
        self.assertEqual(run['argv'][-1], filterset)
        self.assertIn('--no-run', build['argv'])
        self.assertNotIn('--no-fail-fast', build['argv'])
        self.assertIn('--no-fail-fast', run['argv'])
        self.assertNotIn('--no-run', run['argv'])
        self.assertEqual(run['argv'][:5], ['env', 'CARGO_BUILD_JOBS=4', 'python3',
                                         str(Path(__file__).resolve().parent / 'gpu_queue.py'), '--'])
        self.assertEqual(build['argv'][:3], ['env', 'CARGO_BUILD_JOBS=4', 'cargo'])
        self.assertLess([c['name'] for c in checks].index(build['name']),
                        [c['name'] for c in checks].index(run['name']))

    def test_gpu_only_selection_is_not_sent_to_nextest(self):
        cpu_plan = cpu_scope.Plan(gpu_filters={'gpu::smoke::'})
        gpu_plan = gpu_scope.Plan(paths=['crates/fixture/src/lib.rs'])
        plan = self._mocked_plan(cpu_plan, gpu_plan)
        self.assertFalse(any(c['name'].startswith('tests') for c in plan['checks']))
        self.assertIn('clippy', [c['name'] for c in plan['checks']])

    def test_explicit_whole_selection_warns_instead_of_unfiltered_tests(self):
        cpu_plan = cpu_scope.Plan(packages={'fixture'}, whole={'fixture'})
        plan = self._mocked_plan(cpu_plan)
        self.assertFalse(any(c['name'].startswith('tests') for c in plan['checks']))
        self.assertTrue(any('focused validation' in warning for warning in plan['warnings']))

    def test_real_package_and_tooling_scopes(self):
        repo = Path(__file__).resolve().parents[1]
        plan = codex_checks.build_plan(repo, ["scripts/codex_usage.py"])
        self.assertEqual([Path(c["argv"][-1]).name for c in plan["checks"]],
                         ["test_codex_usage.py", "test_dev.py"])
        plan = codex_checks.build_plan(repo, ["crates/manifold-ui/src/param_surface.rs"])
        self.assertIn("manifold-ui", plan["packages"])
        self.assertTrue(all("--manifest-path" in c["argv"] for c in plan["checks"] if "cargo" in c["argv"]))
        self.assertTrue(plan["warnings"])
        plan = codex_checks.build_plan(repo, ["crates/manifold-app/src/ui_bridge/projection/timeline.rs"])
        self.assertEqual([c['name'] for c in plan['checks']],
                         ['clippy', 'tests-build/manifold-app', 'tests/manifold-app', 'ui-flows'])
        with self.assertRaises(RuntimeError):
            codex_checks.build_plan(repo, ["scripts/../../outside"])

    def test_dirty_and_invalid_base(self):
        with tempfile.TemporaryDirectory() as d:
            repo = Path(d)
            subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
            subprocess.run(["git", "config", "user.email", "x@y"], cwd=repo, check=True)
            subprocess.run(["git", "config", "user.name", "x"], cwd=repo, check=True)
            (repo / "a.txt").write_text("a")
            subprocess.run(["git", "add", "a.txt"], cwd=repo, check=True)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=repo, check=True)
            (repo / "a.txt").write_text("b")
            (repo / "new.txt").write_text("n")
            self.assertEqual(set(codex_checks.changed_paths(repo, "HEAD")), {"a.txt", "new.txt"})
            subprocess.run(["git", "mv", "a.txt", "renamed file.txt"], cwd=repo, check=True)
            self.assertEqual(set(codex_checks.changed_paths(repo, "HEAD")), {"a.txt", "renamed file.txt", "new.txt"})
            with self.assertRaises(RuntimeError):
                codex_checks.changed_paths(repo, "missing-ref")


if __name__ == "__main__":
    unittest.main()
