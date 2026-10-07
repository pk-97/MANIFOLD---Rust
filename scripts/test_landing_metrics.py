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
        self.assertEqual(m['slow_tests'], [])
        self.assertIn('none recorded', lm.render(m, 7))

    def test_slow_tests_take_max_and_count_each_landing_once(self):
        rows = [row(self.now, 0, [('tests/1', 'PASS', 50), ('tests/2', 'PASS', 40)]),
                row(self.now, 0, [('tests', 'PASS', 60)])]
        rows[0]['checks'][0]['slow_tests'] = [
            {'name': 'pkg shared', 's': 35}, {'name': 'pkg first', 's': 12.3}]
        rows[0]['checks'][1]['slow_tests'] = [{'name': 'pkg shared', 's': 40}]
        rows[1]['checks'][0]['slow_tests'] = [
            {'name': 'pkg shared', 's': 30}, {'name': 'pkg second', 's': 50}]
        m = lm.summarize(rows, landings=2)
        self.assertEqual(m['slow_tests'], [
            {'name': 'pkg second', 'max_s': 50, 'landings': 1},
            {'name': 'pkg shared', 'max_s': 40, 'landings': 2},
            {'name': 'pkg first', 'max_s': 12.3, 'landings': 1},
        ])
        self.assertIn('pkg shared: 40.000s; 2 landings', lm.render(m, 7))

    def test_slow_tests_limit_and_informational_only(self):
        recent = row(self.now, 0, [('tests', 'PASS', 10000)])
        recent['checks'][0]['slow_tests'] = [
            {'name': f'pkg test_{n}', 's': 1000 + n} for n in range(12)]
        metrics = lm.summarize([recent], landings=1)
        self.assertEqual(len(metrics['slow_tests']), 10)
        self.assertEqual(metrics['slow_tests'][0]['max_s'], 1011)
        self.assertEqual(metrics['slow_tests'][-1]['max_s'], 1002)
        with patch.object(lm, 'read_rows', return_value=[recent]), \
                patch.object(lm, 'landings_since', return_value=1), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(lm.main(['--timings', '/unused']), 0)
        self.assertIn('1011.000s', out.getvalue())
        self.assertNotIn('LANDING METRICS: RED', out.getvalue())

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
