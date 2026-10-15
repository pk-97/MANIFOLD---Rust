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

    def test_one_gate_maintains_caches_once_and_checks_the_reserve_every_leg(self):
        target = self.repo / "target"
        good = BuildCheck(True, target, 200 * 2**30)
        short = BuildCheck(False, target, 10 * 2**30, 30 * 2**30,
                           reason="REFUSED: only 10.0 GiB free", reclaimable=True)
        token = landing_gate.MAINTAINED_TARGETS.set(set())
        try:
            with patch.dict(os.environ, {}, clear=True), \
                    patch("storage_budget.disk_free", return_value=123), \
                    patch("storage_budget.check_build",
                          side_effect=[good, good, short, good]) as check:
                for _ in range(3):
                    _, refusal = landing_gate.build_environment(["cargo", "clippy"], self.repo)
                    self.assertIsNone(refusal)
        finally:
            landing_gate.MAINTAINED_TARGETS.reset(token)
        calls = [(c.args, c.kwargs) for c in check.call_args_list]
        # Full maintenance, a read-only reserve check, then a read-only check
        # that falls short and so maintains again before admitting.
        self.assertEqual(calls, [((target, self.repo), {}),
                                 ((target, self.repo), {"free_bytes": 123}),
                                 ((target, self.repo), {"free_bytes": 123}),
                                 ((target, self.repo), {})])
        # Outside a gate run every admission maintains.
        with patch.dict(os.environ, {}, clear=True), \
                patch("storage_budget.check_build", return_value=good) as check:
            landing_gate.build_environment(["cargo", "clippy"], self.repo)
            landing_gate.build_environment(["cargo", "clippy"], self.repo)
        self.assertEqual([c.kwargs for c in check.call_args_list], [{}, {}])

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

    def test_flow_gate_needs_space_only_when_it_selects_flows(self):
        cmd = ["python3", "scripts/run_ui_flows.py", "--touched", "a...b"]
        admission = BuildCheck(False, self.repo / "target", 0, reason="low space")
        with patch("storage_budget.check_build", return_value=admission) as check, \
                patch("landing_gate.touched_flow_filters", return_value=[]) as select:
            self.assertEqual(landing_gate.build_environment(cmd, self.repo), (None, None))
        select.assert_called_once_with(self.repo, "a...b")
        check.assert_not_called()
        with patch("storage_budget.check_build", return_value=admission) as check, \
                patch("landing_gate.touched_flow_filters", return_value=["drag-clip"]):
            _, refusal = landing_gate.build_environment([*cmd, "--build-only"], self.repo)
        self.assertIn("low space", refusal)
        check.assert_called_once()

    def test_flow_gate_selection_error_still_requires_space(self):
        cmd = ["python3", "scripts/run_ui_flows.py", "--touched", "a...b"]
        admission = BuildCheck(False, self.repo / "target", 0, reason="low space")
        with patch("storage_budget.check_build", return_value=admission), \
                patch("landing_gate.touched_flow_filters", side_effect=SystemExit(2)):
            _, refusal = landing_gate.build_environment(cmd, self.repo)
        self.assertIn("low space", refusal)

    def test_read_only_commands_do_not_require_space(self):
        with patch("storage_budget.check_build") as check:
            self.assertEqual(landing_gate.build_environment(
                ["cargo", "metadata"], self.repo), (None, None))
            self.assertEqual(landing_gate.build_environment(
                ["git", "diff"], self.repo), (None, None))
        check.assert_not_called()


if __name__ == "__main__":
    unittest.main()
