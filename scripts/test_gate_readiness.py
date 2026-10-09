#!/usr/bin/env python3
"""CPU-only readiness and P1 planner contract tests."""
import copy
import json
from pathlib import Path
import re
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import cpu_scope
import gate_readiness
import gpu_scope
import gate_workspace
from gate_workspace import Workspace


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "scripts/fixtures/gate-p1.json"


def p1_snapshot():
    snapshot = json.loads(FIXTURE.read_text())
    metadata = copy.deepcopy(snapshot["metadata"])
    # The checked-in snapshot is repo-relative. Workspace resolves metadata
    # paths, so make both manifests and target sources explicit for this tree.
    for package in metadata["packages"]:
        package["manifest_path"] = str(ROOT / package["manifest_path"])
        for target in package["targets"]:
            target["src_path"] = str(ROOT / target["src_path"])
    metadata["workspace_root"] = str(ROOT)
    return snapshot, Workspace(ROOT, metadata=metadata)


class P1PlannerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.snapshot, cls.workspace = p1_snapshot()
        cls.paths = cls.snapshot["paths"]

    def test_p1_base_marks_new_and_moved_engine_crates_whole(self):
        plan = cpu_scope.plan_for_paths(self.paths, ROOT, self.workspace,
                                        base=self.snapshot["base"])
        counts = self.snapshot['old_cpu_filter_counts']['packages']
        self.assertEqual(sum(counts.values()), 1483)
        for package in counts:
            self.assertIn(package, plan.whole)
            self.assertEqual(plan.selections()[package], f"package(={package})")

    def test_compact_scoped_aggregation_contains_each_old_selection(self):
        rows = self.snapshot['scoped_cases']
        self.assertTrue(rows)
        filters = {f"(package(={row['package']}) & binary(={row['target']}))" for row in rows}
        plan = cpu_scope.Plan(packages={row['package'] for row in rows}, filters=filters)
        # These are actual P1 census entries, with explicit metadata owners.
        identities = {(row['package'], row['target'], row['test']) for row in rows}
        identities.add(('unrelated', rows[0]['target'], rows[0]['test']))

        def selected(expression):
            clauses = expression.split(' | ')
            return {identity for identity in identities if any(
                f'package(={identity[0]})' in clause and f'binary(={identity[1]})' in clause
                for clause in clauses)}

        for package, aggregate in plan.selections().items():
            old_union = set().union(*(selected(expression) for expression in filters
                                      if f'package(={package})' in expression))
            self.assertTrue(old_union)
            self.assertEqual(selected(aggregate), old_union)
            for owner, target, test in old_union:
                self.assertTrue(test)
                self.assertTrue(any(t['name'] == target for t in self.workspace.targets(owner)))

    def test_feature_gated_targets_move_to_gpu_required_binaries(self):
        plan = cpu_scope.plan_for_paths(self.paths, ROOT, self.workspace,
                                        base=self.snapshot["base"])
        actual = {(package['name'], target['name'])
                  for package in self.snapshot['metadata']['packages']
                  for target in package['targets']
                  if 'gpu-proofs' in target.get('required-features', [])}
        self.assertEqual({(row['package'], row['target'])
                          for row in self.snapshot['feature_targets']}, actual)
        expected = {(row['package'], row['target'])
                    for row in self.snapshot['feature_targets']
                    if any(path.endswith(f"tests/{row['target']}.rs")
                           or f"tests/{row['target']}/" in path for path in self.paths)
                    and any(Path(target['src_path']).is_file()
                            for target in self.workspace.targets(row['package'], 'test')
                            if target['name'] == row['target'])}
        self.assertTrue(expected, 'feature-transfer assertions must exercise real targets')
        self.assertEqual(plan.gpu_binaries, expected)
        gpu = gpu_scope.Plan(paths=['fixture'], workspace=Workspace(ROOT),
                             required_binaries=plan.gpu_binaries, glb=True)
        required_runs = {(run['package'], target): run for run in gpu.runs()
                         for target in run['targets'] if run['budgeted']}
        for _, target in expected:
            expression = f"(package(=manifold-nodes) & binary(={target}))"
            self.assertNotIn(expression, plan.filters)
            self.assertEqual(required_runs[('manifold-nodes', target)]['filters'], [])

    def test_scoped_empty_mapping_is_red_even_with_nonempty_union(self):
        listing = {"rust-suites": {
            "smoke": {"binary-name": "smoke", "testcases": ["present::case"]},
        }}
        plan = cpu_scope.Plan(
            packages={"fixture"},
            filters={"(package(=fixture) & test(/^missing::/))",
                     "(package(=fixture) & test(/^present::/))"},
        )
        with self.assertRaisesRegex(ValueError, "ownership mapping resolves to no tests"):
            cpu_scope.validate_inventory(plan, "fixture", listing)

        whole = copy.deepcopy(plan)
        whole.whole.add("fixture")
        self.assertEqual(cpu_scope.validate_inventory(whole, "fixture", listing),
                         {("smoke", "present::case")})

    def test_testless_path_module_widens_only_its_package(self):
        plan = cpu_scope.plan_for_paths([
            "crates/manifold-app/src/frame_time.rs",
            "crates/manifold-gpu/src/metal/device.rs",
        ], ROOT, self.workspace)
        listing = {"rust-suites": {
            "app": {"binary-name": "manifold", "testcases": ["other::case"]},
        }}
        cpu_scope.validate_inventory(plan, "manifold-app", listing)
        self.assertEqual(plan.whole, {"manifold-app"})
        self.assertEqual(plan.selections()["manifold-app"], "package(=manifold-app)")
        self.assertIn("test(/^metal::device::/)", plan.selections()["manifold-gpu"])
        self.assertIn("frame_time.rs has no tests of its own: running manifold-app whole",
                      plan.describe())

    def test_testless_path_does_not_hide_an_explicit_mapping_typo(self):
        expression = "(package(=fixture) & test(/^empty::/))"
        plan = cpu_scope.Plan(packages={"fixture"},
                              filters={expression, "(package(=fixture) & binary(=typo))"},
                              path_filters={expression: {"crates/fixture/src/empty.rs"}})
        listing = {"rust-suites": {
            "smoke": {"binary-name": "smoke", "testcases": ["present::case"]},
        }}
        with self.assertRaisesRegex(ValueError, "ownership mapping resolves to no tests"):
            cpu_scope.validate_inventory(plan, "fixture", listing)
        self.assertFalse(plan.whole)

    def test_explicit_module_mapping_keeps_guard_when_also_path_derived(self):
        path = "crates/manifold-app/src/frame_time.rs"
        row = (path, ".rs", "manifold-app", ["frame_time"], [])
        with patch.object(cpu_scope, "PREFIX_ROWS", [row]):
            plan = cpu_scope.plan_for_paths([path], ROOT, self.workspace)
        listing = {"rust-suites": {
            "app": {"binary-name": "manifold", "testcases": ["other::case"]},
        }}
        with self.assertRaisesRegex(ValueError, "ownership mapping resolves to no tests"):
            cpu_scope.validate_inventory(plan, "manifold-app", listing)

    def test_metadata_failure_is_red_without_a_build_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            with patch.object(gate_readiness, "Workspace",
                              side_effect=ValueError("cargo metadata failed")), \
                    patch("codex_regressions.inventory", return_value=[]):
                result = gate_readiness.plan(repo, ["crates/fixture/src/lib.rs"], "base")
        self.assertIsNone(result["workspace"])
        self.assertIsNone(result["cpu"])
        self.assertIsNone(result["gpu"])
        self.assertIn(("metadata", "cargo metadata failed"), result["errors"])

    def test_surviving_unowned_crate_source_is_red(self):
        workspace = SimpleNamespace(ownership_errors=lambda paths, base: [
            'crates/omitted/src/lib.rs: surviving Rust/manifests has no Cargo workspace owner'])
        with self.assertRaisesRegex(ValueError, 'surviving Rust/manifests'):
            cpu_scope.plan_for_paths(['crates/omitted/src/lib.rs'], ROOT, workspace)

    def test_malformed_metadata_is_normalized_to_value_error(self):
        with self.assertRaisesRegex(ValueError, 'malformed workspace membership'):
            Workspace(ROOT, metadata={'workspace_members': None, 'packages': []})
        with self.assertRaisesRegex(ValueError, 'workspace member missing package'):
            Workspace(ROOT, metadata={'workspace_members': ['missing'], 'packages': []})

    def test_ownership_distinguishes_surviving_omission_from_deleted_package(self):
        workspace = Workspace.__new__(Workspace)
        workspace.roots = {}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(gate_workspace, '_git_package_roots', return_value={'crates/removed'}):
            workspace.repo = Path(directory)
            source = workspace.repo / 'crates/omitted/src/lib.rs'
            source.parent.mkdir(parents=True)
            source.write_text('fn omitted() {}')
            self.assertEqual(len(workspace.ownership_errors(['crates/omitted/src/lib.rs'], 'base')), 1)
            manifest = workspace.repo / 'crates/excluded/Cargo.toml'
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\nname = "excluded"\nversion = "0.1.0"\n')
            self.assertEqual(len(workspace.ownership_errors(['crates/excluded/src/lib.rs'], 'base')), 1)
            self.assertEqual(workspace.ownership_errors(['crates/removed/src/lib.rs'], 'base'), [])

    def test_readiness_collects_reverse_dependency_type_error(self):
        workspace = SimpleNamespace(
            packages={'fixture': {'features': {}}}, roots={'fixture': 'crates/fixture'},
            owner=lambda path: 'fixture',
            ownership_errors=lambda paths, base: [],
            reverse_dependencies=lambda packages: (_ for _ in ()).throw(TypeError('bad dependency shape')),
            targets=lambda package, kind=None: [],
            validate_nextest=lambda: None,
        )
        cpu = cpu_scope.Plan(packages={'fixture'})
        gpu = gpu_scope.Plan()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(gate_readiness, 'Workspace', return_value=workspace), \
                patch.object(gate_readiness.cpu_scope, 'plan_for_paths', return_value=cpu), \
                patch.object(gate_readiness.gpu_scope, 'plan_for_paths', return_value=gpu), \
                patch.object(gate_readiness, 'reference_problems', return_value=[]), \
                patch('codex_regressions.inventory', return_value=[]):
            result = gate_readiness.plan(Path(directory), ['crates/fixture/src/lib.rs'], 'base')
        self.assertIn(('reverse-dependencies', 'bad dependency shape'), result['errors'])

    def test_unexecutable_selected_tooling_entrypoint_is_reported_even_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            script = repo / 'scripts/tool.py'
            script.parent.mkdir()
            script.write_text('#!/usr/bin/env python3\n')
            with patch('codex_checks.tooling_checks', return_value=[{'name': 'scripts/tool.py'}]):
                problems = gate_readiness.executable_problems(repo, ['src/changed.rs'])
        self.assertIn('scripts/tool.py: shebang entrypoint is not executable', problems)

    def test_invalid_flow_manifest_collects_shape_and_reference_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            flow_dir = repo / 'scripts/ui-flows'
            flow_dir.mkdir(parents=True)
            (flow_dir / 'manifest.json').write_text(json.dumps({
                'flows': {'missing': ''},
                'expected_fail': {'duplicate': {'scene': 's'}},
                'unresolved': {'duplicate': ''},
                'path_triggers': {'src/': ['unknown']},
            }))
            (flow_dir / 'orphan.json').write_text('{')
            problems = gate_readiness.flow_problems(repo, ['src/main.rs'])
        self.assertGreaterEqual(len(problems), 4)
        self.assertTrue(any('stale flow entry' in p for p in problems))
        self.assertTrue(any('unknown flow' in p for p in problems))

    def test_imported_helper_is_not_a_direct_entrypoint(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            script = repo / 'scripts/helper.py'
            script.parent.mkdir()
            script.write_text('#!/usr/bin/env python3\ndef helper(): pass\n')
            self.assertEqual(gate_readiness.executable_problems(repo, ['scripts/helper.py'], []), [])

    def test_flow_trigger_substrings_match_runner_semantics(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            flow_dir = repo / 'scripts/ui-flows'
            flow_dir.mkdir(parents=True)
            (flow_dir / 'manifest.json').write_text(json.dumps({
                'flows': {'automation-drag': 'scene'},
                'path_triggers': {'src/': ['automation']},
            }))
            (flow_dir / 'automation-drag.json').write_text('[]')
            self.assertEqual(gate_readiness.flow_problems(repo, ['src/main.rs']), [])

    def test_crate_move_plans_are_inert_for_all_scope_planners(self):
        root = ".claude/orchestration/crate-split/"
        paths = [root + relative for relative in (
            "p2a/templates/crates/manifold-nodes-image/tests/gpu_proofs/main.rs",
            "p2b/templates/crates/manifold-nodes-scene/src/primitives/shaders/example.wgsl",
            "p2c/templates/crates/manifold-compositor/Cargo.toml",
            "future/tests/gpu_proofs/example.rs",
            "future/shaders/example.wgsl",
            "future/moves.tsv",
        )]
        for path in paths:
            with self.subTest(path=path):
                self.assertFalse(gpu_scope.is_gpu_path(path, self.workspace))
        cpu = cpu_scope.plan_for_paths(paths, ROOT, self.workspace)
        self.assertFalse(cpu.packages)
        self.assertFalse(cpu.filters)
        self.assertFalse(cpu.gpu_binaries)
        gpu = gpu_scope.plan_for_paths(paths, ROOT, workspace=self.workspace)
        self.assertFalse(gpu.active)
        self.assertFalse(gpu.filters)
        self.assertFalse(gpu.unmapped)
        self.assertEqual(gpu.runs(), [])
        # Readiness checks the live nextest policy, whose harnesses have moved
        # since this historical P1 metadata snapshot was captured.
        with patch.object(gate_readiness, "Workspace", return_value=Workspace(ROOT)), \
             patch.object(gate_readiness, "selected_tooling", wraps=gate_readiness.selected_tooling) as tooling:
            ready = gate_readiness.plan(ROOT, paths)
        tooling.assert_called_once_with(ROOT, [])
        self.assertFalse(ready['errors'])
        self.assertFalse(ready['packages'])
        self.assertFalse(ready['cpu'].packages)
        self.assertFalse(ready['gpu'].active)
        self.assertTrue(gpu_scope.is_gpu_path(
            "crates/manifold-nodes-scene/tests/gpu_proofs/main.rs", self.workspace))
        self.assertTrue(gpu_scope.is_gpu_path(
            ".claude/orchestration/crate-split-other/shaders/example.wgsl", self.workspace))

    def test_new_gpu_package_needs_explicit_default_test_group_ownership(self):
        workspace = Workspace(ROOT)
        original = workspace.nextest_gpu_filter()
        workspace.packages['synthetic-leaf'] = {'features': {'gpu-proofs': []}, 'targets': []}
        with self.assertRaisesRegex(ValueError, 'ownership unresolved'):
            workspace.nextest_gpu_filter()
        del workspace.packages['synthetic-leaf']
        self.assertEqual(workspace.nextest_gpu_filter(), original)

    def test_readiness_aggregates_stale_nextest_regression_and_reference_reds(self):
        workspace = SimpleNamespace(
            packages={"fixture": {"features": {}}},
            roots={"fixture": "crates/fixture"},
            owner=lambda path: "fixture",
            ownership_errors=lambda paths, base: [],
            reverse_dependencies=lambda packages: [],
            targets=lambda package, kind=None: [],
            validate_nextest=lambda: (_ for _ in ()).throw(
                ValueError("nextest GPU grouping has stale owner")),
        )
        cpu = cpu_scope.Plan(
            packages={"fixture"},
            filters={"(package(=fixture) & test(/^present::/))"},
        )
        gpu = gpu_scope.Plan()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(gate_readiness, "Workspace", return_value=workspace), \
                patch.object(gate_readiness.cpu_scope, "plan_for_paths", return_value=cpu) as cpu_mock, \
                patch.object(gate_readiness.gpu_scope, "plan_for_paths", return_value=gpu) as gpu_mock, \
                patch.object(gate_readiness, "reference_problems",
                             return_value=["missing fixture asset"]), \
                patch("codex_regressions.inventory",
                      side_effect=ValueError("regression inventory missing")):
            result = gate_readiness.plan(Path(directory),
                                         ["crates/fixture/src/lib.rs"], "base")
        errors = result["errors"]
        labels = [label for label, _ in errors]
        messages = [message for _, message in errors]
        self.assertIn("nextest-grouping", labels)
        self.assertIn("references", labels)
        self.assertIn("regression-inventory", labels)
        self.assertIn("nextest GPU grouping has stale owner", messages)
        self.assertIn("missing fixture asset", messages)
        self.assertIn("regression inventory missing", messages)

        self.assertIs(result['gpu'], gpu)
        self.assertEqual(cpu_mock.call_count, 1)
        self.assertIs(gpu_mock.call_args.kwargs['cpu_plan'], cpu)


if __name__ == "__main__":
    unittest.main()
