#!/usr/bin/env python3
"""Storage admission at actual landing-gate subprocess boundaries."""
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

    def test_read_only_commands_do_not_require_space(self):
        with patch("storage_budget.check_build") as check:
            self.assertEqual(landing_gate.build_environment(
                ["cargo", "metadata"], self.repo), (None, None))
            self.assertEqual(landing_gate.build_environment(
                ["git", "diff"], self.repo), (None, None))
        check.assert_not_called()


if __name__ == "__main__":
    unittest.main()
