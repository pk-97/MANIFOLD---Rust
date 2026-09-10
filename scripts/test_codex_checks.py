#!/usr/bin/env python3
import tempfile
import unittest
import subprocess
from pathlib import Path
import sys
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).parent))

import codex_checks
import landing_gate
import run_ui_flows


class PlannerTests(unittest.TestCase):
    def test_ui_trigger_matching_and_flow_file(self):
        manifest = {"path_triggers": {"crates/manifold-ui/src/panels/rt_quality_panel.rs": ["rt-quality"]}}
        filters, hits = run_ui_flows.filters_for_paths(
            ["crates/manifold-ui/src/panels/rt_quality_panel.rs", "scripts/ui-flows/foo.json"], manifest)
        self.assertEqual(filters, ["foo", "rt-quality"])
        self.assertEqual(hits["scripts/ui-flows/foo.json"], ["foo"])

    def test_gpu_scope_union_and_full_for_uncovered(self):
        paths = ["crates/manifold-gpu/src/metal/raytrace.rs", "crates/manifold-renderer/src/node_graph/freeze/x.rs"]
        self.assertEqual(landing_gate.gpu_proofs_scope_for_paths(paths), (["freeze::", "rt_"], ["particletext"]))
        self.assertIsNone(landing_gate.gpu_proofs_scope_for_paths(paths + ["crates/manifold-gpu/src/foo.rs"]))

    def test_plan_has_exact_tool_commands(self):
        with tempfile.TemporaryDirectory() as d:
            repo = Path(d)
            (repo / "scripts/ui-flows").mkdir(parents=True)
            (repo / "scripts/ui-flows/manifest.json").write_text('{"path_triggers": {}}')
            with patch("codex_regressions.inventory", return_value=[]):
                plan = codex_checks.build_plan(repo, ["scripts/run_ui_flows.py"])
            self.assertEqual(plan["checks"][0]["argv"], ["python3", "-B", str(repo.resolve() / "scripts/test_codex_checks.py")])

    def test_real_package_and_tooling_scopes(self):
        repo = Path(__file__).resolve().parents[1]
        plan = codex_checks.build_plan(repo, ["scripts/codex_usage.py"])
        self.assertEqual(len(plan["checks"]), 1)
        self.assertTrue(plan["checks"][0]["argv"][-1].endswith("test_codex_usage.py"))
        plan = codex_checks.build_plan(repo, ["crates/manifold-ui/src/param_surface.rs"])
        self.assertIn("manifold-ui", plan["packages"])
        self.assertTrue(all("--manifest-path" in c["argv"] for c in plan["checks"] if c["argv"][0] == "cargo"))
        self.assertTrue(plan["warnings"])
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
