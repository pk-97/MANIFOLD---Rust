#!/usr/bin/env python3
"""Storage admission at actual landing-gate subprocess boundaries."""
import io
import os
import unittest
from pathlib import Path
from unittest.mock import patch

import landing_gate
from storage_budget import BuildCheck


class StorageGateTests(unittest.TestCase):
    def setUp(self):
        self.repo = Path(__file__).resolve().parents[1]

    def test_low_space_stops_subprocess(self):
        admission = BuildCheck(False, self.repo / "target", 0, reason="low space")
        with patch("storage_budget.check_build", return_value=admission), \
                patch("landing_gate.subprocess.Popen") as execute:
            result = landing_gate.run_cmd(["cargo", "clippy"], self.repo, 10)
        self.assertEqual(result[0], 2)
        self.assertIn("low space", result[2])
        execute.assert_not_called()

    def test_allowed_build_pins_target_for_nested_cargo(self):
        admission = BuildCheck(True, self.repo / "target", 200 * 2**30)
        with patch.dict(os.environ, {}, clear=True), \
                patch("storage_budget.check_build", return_value=admission) as check:
            env, refusal = landing_gate.build_environment(
                ["python3", "scripts/gpu_proofs_gate.py"], self.repo)
        self.assertIsNone(refusal)
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.repo / "target"))
        check.assert_called_once_with(self.repo / "target", self.repo)

    def test_override_is_checked_and_conflicts_refused(self):
        admission = BuildCheck(False, Path("/private/tmp/unmanaged"), 0,
                               reason="unmanaged target")
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(admission.target)}, clear=True), \
                patch("storage_budget.check_build", return_value=admission) as check:
            _, refusal = landing_gate.build_environment(["cargo", "nextest"], self.repo)
        self.assertIn("unmanaged target", refusal)
        check.assert_called_once_with(admission.target, self.repo)
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": "target",
                                     "CARGO_BUILD_TARGET_DIR": "other"}, clear=True):
            _, refusal = landing_gate.build_environment(["cargo", "test"], self.repo)
        self.assertIn("conflicting", refusal)

    def test_reserve_shortfall_reclaims_then_admits(self):
        short = BuildCheck(False, self.repo / "target", 10 * 2**30, 100 * 2**30,
                           reason="REFUSED: only 10.0 GiB free", reclaimable=True)
        good = BuildCheck(True, self.repo / "target", 200 * 2**30)
        with patch.dict(os.environ, {}, clear=True), \
                patch("storage_budget.check_build", side_effect=[short, good]) as check, \
                patch("landing_gate.reclaim_landed_caches", return_value="\nRECLAIMED slot-0") as reclaim:
            env, refusal = landing_gate.build_environment(["cargo", "clippy"], self.repo)
        self.assertIsNone(refusal)
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.repo / "target"))
        reclaim.assert_called_once_with(100 * 2**30)
        self.assertEqual(check.call_count, 2)

    def test_reserve_shortfall_still_refuses_when_reclaim_is_not_enough(self):
        short = BuildCheck(False, self.repo / "target", 10 * 2**30, 100 * 2**30,
                           reason="REFUSED: only 10.0 GiB free", reclaimable=True)
        with patch("storage_budget.check_build", return_value=short), \
                patch("landing_gate.reclaim_landed_caches", return_value="\nKEEP slot-2: dirty") as reclaim, \
                patch("landing_gate.subprocess.Popen") as execute:
            result = landing_gate.run_cmd(["cargo", "nextest", "run"], self.repo, 10)
        self.assertEqual(result[0], 2)
        self.assertIn("only 10.0 GiB free", result[2])
        self.assertIn("KEEP slot-2", result[2])
        reclaim.assert_called_once()
        execute.assert_not_called()

    def test_non_reserve_refusals_never_reclaim(self):
        unmanaged = BuildCheck(False, Path("/private/tmp/unmanaged"), 0, reason="REFUSED: unmanaged")
        with patch("storage_budget.check_build", return_value=unmanaged), \
                patch("landing_gate.reclaim_landed_caches") as reclaim:
            _, refusal = landing_gate.build_environment(["cargo", "build"], self.repo)
        self.assertIn("unmanaged", refusal)
        reclaim.assert_not_called()

    def _gate(self, refusal, argv_extra):
        def fake_run_cmd(cmd, cwd, timeout, live_log=None):
            if "merge-base" in cmd:
                return 0, "base\n", "", 0
            if "rev-parse" in cmd:
                return 0, "head\n", "", 0
            return 0, "", "", 0

        readiness = {"packages": ["manifold-core"], "dependents": [], "cpu": None,
                     "gpu": None, "errors": [], "tooling": [], "flows": False}
        with patch("sys.argv", ["landing_gate.py", "--repo", str(self.repo), *argv_extra]), \
                patch("landing_gate.run_cmd", side_effect=fake_run_cmd), \
                patch("landing_gate.diff_scope.effective_paths",
                      return_value=(["crates/manifold-core/src/lib.rs"], [])), \
                patch("landing_gate.gate_readiness.plan", return_value=readiness), \
                patch("landing_gate.freshness_problems", return_value=[]), \
                patch("landing_gate.storage_preflight", return_value=refusal) as preflight, \
                patch("landing_gate.run_check") as legs, \
                patch("landing_gate.build_leg") as builds, \
                patch("sys.stdout", new_callable=io.StringIO) as out:
            code = landing_gate.main()
        return code, out.getvalue(), preflight, legs, builds

    def test_full_disk_is_one_refusal_before_any_leg(self):
        refusal = ("Storage admission refused: REFUSED: only 3.0 GiB free; reserve is 50 GiB\n"
                   "RECLAIM SHORT: 47.0G more needed\n"
                   "UNRECLAIMABLE slot-2: 30.1G target, unlanded\n"
                   "UNRECLAIMABLE slot-1: 12.0G target, dirty")
        code, text, preflight, legs, builds = self._gate(refusal, ["--cheap"])
        self.assertNotEqual(code, 0)
        preflight.assert_called_once()
        legs.assert_not_called()
        builds.assert_not_called()
        self.assertEqual(text.count("FAIL storage"), 1, text)
        self.assertEqual(text.count("REFUSED"), 1, text)
        self.assertIn("47.0G more needed", text)
        self.assertIn("UNRECLAIMABLE slot-2: 30.1G target, unlanded", text)

    def test_mid_run_refusal_after_admission_stops_the_gate_once(self):
        short = BuildCheck(False, self.repo / "target", 0, reason="REFUSED: disk filled")
        token = landing_gate.STORAGE_ADMITTED.set(True)
        try:
            with patch("storage_budget.check_build", return_value=short), \
                    patch("landing_gate.subprocess.Popen") as execute:
                with self.assertRaises(landing_gate.StorageStop):
                    landing_gate.run_cmd(["cargo", "clippy"], self.repo, 10)
            execute.assert_not_called()
        finally:
            landing_gate.STORAGE_ADMITTED.reset(token)

    def test_preflight_uses_build_admission_with_reclaim(self):
        short = BuildCheck(False, self.repo / "target", 10 * 2**30, 100 * 2**30,
                           reason="REFUSED: only 10.0 GiB free", reclaimable=True)
        tail = "\nRECLAIM SHORT: 90.0G more needed\nUNRECLAIMABLE slot-3: 40.0G target, dirty"
        with patch("storage_budget.check_build", return_value=short), \
                patch("landing_gate.reclaim_landed_caches", return_value=tail) as reclaim:
            refusal = landing_gate.storage_preflight(self.repo)
        reclaim.assert_called_once_with(100 * 2**30)
        self.assertIn("90.0G more needed", refusal)
        self.assertIn("UNRECLAIMABLE slot-3", refusal)

    def test_read_only_commands_do_not_require_space(self):
        with patch("storage_budget.check_build") as check:
            self.assertEqual(landing_gate.build_environment(
                ["cargo", "metadata"], self.repo), (None, None))
            self.assertEqual(landing_gate.build_environment(
                ["git", "diff"], self.repo), (None, None))
        check.assert_not_called()


if __name__ == "__main__":
    unittest.main()
