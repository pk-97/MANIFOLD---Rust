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

    def test_slow_marker_rows_are_dropped(self):
        recent = row(self.now, 0, [('tests', 'PASS', 100)])
        recent['checks'][0]['slow_tests'] = [
            {'name': '(────) pkg real', 's': 60.0}, {'name': 'pkg real', 's': 45.0}]
        self.assertEqual(lm.summarize([recent], 1)['slow_tests'],
                         [{'name': 'pkg real', 'max_s': 45.0, 'landings': 1}])

    def test_attempts_report_landing_time_percentiles_and_split(self):
        def attempt(minutes, outcome='landed', **extra):
            start = self.now
            return {'branch': 'b', 'start': start.isoformat(),
                    'end': (start + timedelta(minutes=minutes)).isoformat(), 'outcome': outcome,
                    'gate_runs': 1, 'moved_main_regates': 0, 'lock_wait_s': 2.0, 'gpu_wait_s': None,
                    **extra}
        attempts = [attempt(m) for m in (10, 20, 30, 40, 50, 60, 70, 80, 90, 100)]
        attempts += [attempt(500, 'red', gpu_wait_s=30.0), attempt(1, 'cancelled', moved_main_regates=2)]
        m = lm.summarize_attempts(attempts)
        self.assertEqual((m['landing_median_min'], m['landing_p90_min']), (55.0, 100.0))
        self.assertEqual(m['outcomes'], {'landed': 10, 'red': 1, 'cancelled': 1})
        self.assertEqual((m['attempt_gate_runs'], m['attempt_moved_main_regates'],
                          m['attempt_lock_wait_s'], m['attempt_gpu_wait_s']), (12, 2, 24, 30))
        text = lm.render({**lm.summarize(self.rows(), 2), **m}, 7)
        self.assertIn('median 55.0 min, p90 100.0 min', text)
        self.assertIn('moved-main re-gates 2', text)
        with tempfile.TemporaryDirectory() as d:
            timings = Path(d) / 'landing-gate-timings.jsonl'
            self.assertEqual(lm.read_attempts(timings, self.now), [])
            timings.with_name('landing-runs.jsonl').write_text(
                '\n'.join(json.dumps(a) for a in attempts) + '\n')
            self.assertEqual(len(lm.read_attempts(timings, self.now - timedelta(days=1))), 12)

    def red(self, label, detail=None, duration=100):
        r = row(self.now, 1, [("clippy", "PASS", 5), (label, "FAIL", duration)])
        if detail:
            r["checks"][1]["detail"] = detail
        return r

    def test_each_red_class_from_a_minimal_row(self):
        cases = [
            ("storage refusal", self.red("tests-build/manifold-app", "Storage admission refused: REFUSED")),
            ("flaky", self.red("tests", "FLAKY consolidated_peel passed on retry")),
            ("real code red", self.red("clippy/manifold-ui")),
            ("mapping gap", self.red("test-ownership/manifold-app")),
            ("merge conflict", self.red("stable-tree", "CONFLICT (content): Merge conflict in x")),
            ("missing gate input", self.red("deny")),
            ("missing gate input", self.red("docs-index")),
            ("GPU reset", self.red("gpu-proofs", "kIOGPUCommandBufferCallbackErrorInnocentVictim")),
            ("GPU reset", self.red("gpu-proofs", "kIOGPUCommandBufferCallbackErrorHang")),
            ("GPU budget only", self.red("gpu-proofs", "GPU-PROOFS TIMING: FAIL 1073s of 360s")),
        ]
        for expected, r in cases:
            self.assertEqual(lm.classify_red(r), expected, r["checks"][1])

    def test_unrecorded_cause_is_unclassified_never_guessed(self):
        self.assertEqual(lm.classify_red(self.red("gpu-proofs", duration=983.7)), "unclassified")
        self.assertEqual(lm.classify_red(self.red("stable-tree")), "unclassified")
        self.assertEqual(lm.classify_red(row(self.now, 1, [("tests", "PASS", 1)])), "unclassified")

    def test_labels_alone_never_guess(self):
        fast = row(self.now, 1, [("tests-build/manifold-app", "FAIL", 0.8),
                                 ("tests-build/manifold-core", "FAIL", 0.6),
                                 ("tests-build/manifold-editing", "FAIL", 0.5)])
        self.assertEqual(lm.classify_red(fast), "unclassified")
        self.assertEqual(lm.classify_red(self.red("flow-gate", duration=3600.1)), "unclassified")
        mixed = row(self.now, 1, [("clippy/x", "FAIL", 5), ("test-ownership/y", "FAIL", 5)])
        self.assertEqual(lm.classify_red(mixed), "unclassified")

    def test_empty_fallback_diff_is_unknown(self):
        self.assertIsNone(lm.branch_paths("lane/e", {}, lambda *a: ""))
        self.assertIsNone(lm.branch_paths("lane/e", {"lane/e": "abc"}, lambda *a: "\n"))

    def test_red_hours_per_class_and_render(self):
        rows = [self.red("clippy", duration=3600 - 5), self.red("deny", duration=1800 - 5),
                self.red("gpu-proofs", duration=1800 - 5), row(self.now, 0, [("tests", "PASS", 9)])]
        classes = lm.summarize(rows, 3)["red_classes"]
        self.assertEqual(classes["real code red"], {"runs": 1, "hours": 1.0})
        self.assertEqual(classes["missing gate input"], {"runs": 1, "hours": 0.5})
        self.assertEqual(classes["unclassified"], {"runs": 1, "hours": 0.5})
        text = lm.render(lm.summarize(rows, 3), 7)
        self.assertIn("red runs by cause", text)
        self.assertIn("real code red: 1 runs, 1.0 h", text)

    def test_dead_renderer_crate_rows_are_dropped_from_slowest(self):
        r = row(self.now, 0, [("tests", "PASS", 100)])
        r["checks"][0]["slow_tests"] = [{"name": "manifold-renderer node_graph::x", "s": 90.0},
                                        {"name": "manifold-nodes real", "s": 45.0}]
        self.assertEqual([t["name"] for t in lm.summarize([r], 1)["slow_tests"]],
                         ["manifold-nodes real"])

    def test_python_rust_split_of_landing_time(self):
        def attempt(branch, minutes):
            return {"branch": branch, "start": self.now.isoformat(), "outcome": "landed",
                    "end": (self.now + timedelta(minutes=minutes)).isoformat()}
        files = {"py": ["scripts/a.py", "docs/x.md"], "rs": ["scripts/a.py", "crates/c/src/lib.rs"],
                 "gone": None}
        split = lm.split_landing_time(
            [attempt("py", 2), attempt("py", 4), attempt("rs", 30), attempt("gone", 99),
             {**attempt("py", 500), "outcome": "red"}],
            lambda branch: files[branch])
        self.assertEqual(split["python_only"], {"landings": 2, "median_min": 3.0})
        self.assertEqual(split["rust"], {"landings": 1, "median_min": 30.0})
        self.assertEqual(split["unknown"], {"landings": 1, "median_min": 99.0})
        text = lm.render({**lm.summarize(self.rows(), 2), "split": split}, 7)
        self.assertIn("python-only 2 landings, median 3.0 min", text)
        self.assertIn("unknown 1", text)

    def test_branch_paths_prefer_merge_commit_then_ref_then_none(self):
        merges = {"lane/x": "abc123"}
        calls = []
        def git(*args):
            calls.append(args)
            return "scripts/a.py\n"
        self.assertEqual(lm.branch_paths("lane/x", merges, git), ["scripts/a.py"])
        self.assertEqual(calls[0], ("diff", "--name-only", "abc123^1", "abc123"))
        self.assertEqual(lm.branch_paths("lane/y", {}, lambda *a: "scripts/b.py\n" if "origin/lane/y" in a[-1] else None),
                         ["scripts/b.py"])
        self.assertIsNone(lm.branch_paths("lane/z", {}, lambda *a: None))

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
