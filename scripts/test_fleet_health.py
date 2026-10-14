#!/usr/bin/env python3
"""Tests for scripts/fleet_health.py: the nightly alarm."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fleet_health  # noqa: E402


class NightlyAlarmTests(unittest.TestCase):
    def _dir(self, log_age_hours, launchd_text=None):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        if log_age_hours is not None:
            p = root / "2026-10-10.log"
            p.write_text("ok")
            t = 1_800_000_000 - log_age_hours * 3600
            import os
            os.utime(p, (t, t))
        if launchd_text is not None:
            (root / "launchd.log").write_text(launchd_text)
        return root

    def test_stale_log_alarms(self):
        problems = []
        fleet_health.check_nightly(problems, 1_800_000_000, self._dir(40))
        self.assertTrue(any("trunk-health" in m and lvl == "FAIL" for lvl, m in problems))

    def test_fresh_log_is_quiet(self):
        problems = []
        fleet_health.check_nightly(problems, 1_800_000_000, self._dir(10, "fine\n"))
        self.assertEqual(problems, [])

    def test_launchd_traceback_alarms(self):
        problems = []
        fleet_health.check_nightly(problems, 1_800_000_000, self._dir(1, "Traceback (most recent call last):\n  x\nModuleNotFoundError: tomllib\n"))
        self.assertTrue(any("crash" in m for _, m in problems))

    def test_launchd_recovery_lines_after_traceback_do_not_alarm(self):
        tb = "Traceback (most recent call last):\n  x\nModuleNotFoundError: tomllib\n"
        for line in ("[trunk-health] deferred: nightly GPU reservation held by x\n",
                     "[trunk-health] origin/main @ abc1234\n"):
            problems = []
            fleet_health.check_nightly(problems, 1_800_000_000, self._dir(1, tb + line))
            self.assertEqual(problems, [], line)


if __name__ == "__main__":
    unittest.main()
