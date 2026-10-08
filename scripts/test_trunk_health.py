#!/usr/bin/env python3
"""Fake-only tests for nightly reservation deferral."""

import sys
import tempfile
import contextlib
import io
from pathlib import Path
import unittest
from unittest.mock import patch

import trunk_health


class ReservationTests(unittest.TestCase):
    def test_run_cmd_refuses_reserved_cargo_before_launch(self):
        with patch.object(trunk_health.gpu_queue, "run_admitted", return_value=None):
            with self.assertRaises(trunk_health.ReservationDeferred):
                trunk_health.run_cmd(["cargo", "clippy"], Path("."), 10)

    def test_nested_feature_defer_stops_run_and_persists_log_without_beads(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            calls = []

            def fake_run(cmd, **kwargs):
                calls.append(cmd)
                if any("feature_matrix.py" in arg for arg in cmd):
                    return 0, "[DEFER] feature matrix: nightly GPU reservation became active\n", "", 0
                return 0, "tip", "", 0

            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["trunk_health.py"]))
            stack.enter_context(patch.object(trunk_health, "LOG_DIR", Path(d)))
            stack.enter_context(patch.object(trunk_health, "missing_tools", return_value=[]))
            stack.enter_context(patch.object(trunk_health, "cap_main_target", return_value=""))
            stack.enter_context(patch.object(trunk_health.gpu_queue, "reservation", return_value={}))
            runs = stack.enter_context(patch.object(trunk_health, "run_cmd", side_effect=fake_run))
            beads = stack.enter_context(patch.object(trunk_health.subprocess, "run",
                                                      side_effect=AssertionError("bead command ran")))
            self.assertEqual(trunk_health.main(), 0)

            self.assertGreater(runs.call_count, 0)
            self.assertEqual(beads.call_count, 0)
            logs = list(Path(d).glob("*.log"))
            self.assertEqual(len(logs), 1)
            self.assertIn("[DEFER] feature matrix", logs[0].read_text())

    def test_live_reservation_defers_before_fetch_or_build(self):
        info = {"owner": "water-campaign", "reason": "GPU proof window",
                "end_epoch": 1_800_000_000}
        with patch.object(trunk_health.gpu_queue, "reservation", return_value=info), \
                patch.object(trunk_health, "run_cmd", side_effect=AssertionError("ran a gate")), \
                patch.object(sys, "argv", ["trunk_health.py", "--dry-run"]):
            self.assertEqual(trunk_health.main(), 0)

    def test_reservation_set_during_run_stops_before_builds_or_gpu(self):
        info = {'owner': 'campaign', 'reason': 'proof window', 'end_epoch': 1800000000}
        # Initial admission + three cheap scripts precede clippy; seven CPU
        # legs precede the first GPU hold. All commands below are fakes.
        for allowed_checks, expected_runs in ((4, 5), (8, 9)):
            with self.subTest(allowed_checks=allowed_checks), tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                stack.enter_context(patch.object(sys, 'argv', ['trunk_health.py']))
                stack.enter_context(patch.object(trunk_health, 'LOG_DIR', Path(d)))
                stack.enter_context(patch.object(trunk_health, 'missing_tools', return_value=[]))
                stack.enter_context(patch.object(trunk_health, 'cap_main_target', return_value=''))
                stack.enter_context(patch.object(trunk_health.gpu_queue, 'reservation',
                                                side_effect=[{}] * allowed_checks + [info]))
                runs = stack.enter_context(patch.object(trunk_health, 'run_cmd', return_value=(0, 'tip', '', 0)))
                hold = stack.enter_context(patch.object(trunk_health.gpu_queue, 'hold'))
                self.assertEqual(trunk_health.main(), 0)
                self.assertEqual(runs.call_count, expected_runs)
                hold.assert_not_called()
                self.assertIn('deferred:', next(Path(d).glob('*.log')).read_text())

    def test_reservation_message_contains_owner_reason_and_expiry(self):
        info = {"owner": "owner", "reason": "reason", "end_epoch": 1_800_000_000}
        line = trunk_health.reservation_message(info)
        self.assertIn("owner", line)
        self.assertIn("reason", line)
        self.assertIn("1800000000", line)


if __name__ == "__main__":
    unittest.main()
