#!/usr/bin/env python3
"""The arithmetic behind the nightly landing-metrics line, on synthetic rows."""
import contextlib
import io
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

import landing_metrics as lm


def row(ts, failed, checks, wait=None):
    return {"ts": ts.isoformat(), "branch": "b", "failed": failed, "gpu_wait_s": wait,
            "checks": [{"label": l, "status": s, "duration_s": d, "ts": ts.isoformat()}
                       for l, s, d in checks]}


class MetricsTests(unittest.TestCase):
    now = datetime(2026, 10, 7, tzinfo=timezone.utc)

    def rows(self):
        return [
            row(self.now, 1, [("gpu-proofs", "FAIL", 400), ("flow-gate", "FAIL", 3600)], wait=1800),
            row(self.now, 0, [("gpu-proofs", "REUSED", None), ("tests", "PASS", 30)], wait=0),
            row(self.now, 0, [("gpu-proofs", "PASS", 200), ("gpu-proofs-build", "PASS", 60)]),
        ]

    def test_summary_counts_runs_reds_reuse_and_waits(self):
        m = lm.summarize(self.rows(), landings=2)
        self.assertEqual((m["gate_runs"], m["landings"], m["runs_per_landing"]), (3, 2, 1.5))
        self.assertEqual(m["red_share"], 0.33)
        self.assertEqual((m["gpu_proofs_runs"], m["gpu_proofs_reuse_share"]), (3, 0.33))
        self.assertEqual(m["gpu_proofs_median_s"], 300)
        self.assertEqual((m["gpu_wait_total_s"], m["gpu_wait_runs_logged"]), (1800, 2))
        self.assertEqual(m["flow_gate_over_10min"], 1)
        self.assertEqual(list(m["leg_hours"])[0], "flow-gate")

    def test_no_landings_reports_without_dividing(self):
        m = lm.summarize(self.rows(), landings=0)
        self.assertIsNone(m["runs_per_landing"])
        self.assertIn("runs per landing None", lm.render(m, 7))

    def test_window_and_red_threshold(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "t.jsonl"
            old = row(self.now - timedelta(days=30), 1, [("tests", "FAIL", 1)])
            recent = [row(self.now - timedelta(days=1), 1, [("tests", "FAIL", 1)]) for _ in range(6)]
            path.write_text("\n".join(json.dumps(r) for r in [old, *recent]) + "\n")
            out = io.StringIO()
            with patch.object(lm, "landings_since", return_value=2), \
                    patch.object(lm, "datetime") as clock, contextlib.redirect_stdout(out):
                clock.now.return_value = self.now
                clock.fromisoformat = datetime.fromisoformat
                code = lm.main(["--timings", str(path), "--days", "7"])
            self.assertEqual(code, 1)
            self.assertIn("gate runs 6", out.getvalue())
            self.assertIn("LANDING METRICS: RED", out.getvalue())

    def test_tooling_errors_fail_open(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = lm.main(["--timings", "/nonexistent/t.jsonl"])
        self.assertEqual(code, 0)
        self.assertIn("skipped", out.getvalue())


if __name__ == "__main__":
    unittest.main()
