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


def projected_match(expression, identity):
    """Match a census identity under its package-stripped projection.

    The P1 census intentionally omits package owners. Binary names and test
    expressions still provide a conservative projection for inclusion checks;
    this does not invent exact nextest ownership.
    """
    binary = re.search(r"binary\(=([^)]*)\)", expression)
    if binary and identity.split("::", 1)[0] != binary[1]:
        return False
    prefix = re.search(r"test\(/(.*)/\)", expression)
    if prefix and not re.search(prefix[1], identity):
        return False
    literal = re.search(r"test\(([^/)][^)]*)\)", expression)
    return not literal or literal[1] in identity


def projected_identities(expressions, identities):
    return {identity for identity in identities
            if any(projected_match(expression, identity) for expression in expressions)}


class P1PlannerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.snapshot, cls.workspace = p1_snapshot()
        cls.paths = cls.snapshot["paths"]
        cls.identities = list(cls.snapshot["census"]["identities"])

    def test_p1_base_marks_new_and_moved_engine_crates_whole(self):
        plan = cpu_scope.plan_for_paths(self.paths, ROOT, self.workspace,
                                        base=self.snapshot["base"])
        self.assertEqual(len(self.snapshot["old_cpu_filters"]), 1483)
        self.assertIn("manifold-node-engine", plan.whole)
        self.assertIn("manifold-renderer", plan.whole)
        for package in ("manifold-node-engine", "manifold-renderer"):
            self.assertEqual(plan.selections()[package], f"package(={package})")

    def test_aggregate_projection_contains_union_of_old_filters(self):
        plan = cpu_scope.plan_for_paths(self.paths, ROOT, self.workspace,
                                        base=self.snapshot["base"])
        packages = sorted({re.search(r"package\(=([^)]*)", expression)[1]
                           for expression in self.snapshot["old_cpu_filters"]})
        for package in packages:
            with self.subTest(package=package):
                old = [expression for expression in self.snapshot["old_cpu_filters"]
                       if f"package(={package})" in expression]
                new = plan.selections()[package].split(' | ')
                old_selected = projected_identities(old, self.identities)
                new_selected = projected_identities(new, self.identities)
                self.assertLessEqual(
                    old_selected, new_selected,
                    f"{package}: aggregate projection narrowed the old filter union")

    def test_feature_gated_targets_move_to_gpu_required_binaries(self):
        plan = cpu_scope.plan_for_paths(self.paths, ROOT, self.workspace,
                                        base=self.snapshot["base"])
        expected = {("manifold-renderer", "glb_conformance"),
                    ("manifold-renderer", "gpu_proofs")}
        self.assertEqual(plan.gpu_binaries, expected)
        gpu = gpu_scope.Plan(paths=['fixture'], workspace=self.workspace,
                             required_binaries=plan.gpu_binaries, glb=True)
        required_runs = {(run['package'], target): run for run in gpu.runs()
                         for target in run['targets']}
        for _, target in expected:
            expression = f"(package(=manifold-renderer) & binary(={target}))"
            self.assertNotIn(expression, plan.filters)
            self.assertEqual(projected_identities([expression], self.identities), set())
            self.assertEqual(required_runs[('manifold-renderer', target)]['filters'], [])

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

    def test_new_gpu_package_needs_explicit_default_test_group_ownership(self):
        workspace = copy.deepcopy(self.workspace)
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
                patch.object(gate_readiness.cpu_scope, "plan_for_paths", return_value=cpu), \
                patch.object(gate_readiness.gpu_scope, "plan_for_paths", return_value=gpu), \
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


if __name__ == "__main__":
    unittest.main()
