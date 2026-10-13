#!/usr/bin/env python3
"""scripts/status.py blocks from fake inputs, a failing source, and the ETA math."""
import json
import tempfile
import threading
import time
import unittest
from pathlib import Path

import status


class BlockTests(unittest.TestCase):
    def test_lanes_block_caps_lines_and_keeps_summary(self):
        out = "\n".join(f"WARN item {i}" for i in range(10)) + "\nfleet-health: 2 Astra running, 10 item(s)\n"
        lines = status.lanes_block(lambda cmd, t: out)
        self.assertEqual(lines[0], "lanes: fleet-health: 2 Astra running, 10 item(s)")
        self.assertLessEqual(len(lines), 1 + status.MAX_DETAIL)

    def test_gpu_block_first_line(self):
        self.assertEqual(status.gpu_block(lambda cmd, t: "GPU free\nmore\n"), ["gpu: GPU free"])

    def test_slots_block_counts_states(self):
        out = ("CACHE: cap 160G\nHUMAN    slot-0 a\nIN-USE   slot-1 b\nIN-USE   slot-2 c\n"
               "FREE     slot-3 d\n")
        self.assertEqual(status.slots_block(lambda cmd, t: out), ["slots: 4 total, FREE 1, HUMAN 1, IN-USE 2"])

    def test_slots_block_calls_list_fast(self):
        seen = []
        status.slots_block(lambda cmd, t: seen.append(cmd) or "")
        self.assertIn("--fast", seen[0])
        self.assertEqual(seen[0][-2:], ["list", "--fast"])

    def test_gate_block_none_running(self):
        self.assertEqual(status.gate_block([], now=100, timings=[]), ["gate: none running"])

    def test_gate_block_running_leg_with_eta(self):
        with tempfile.TemporaryDirectory() as d:
            logs = Path(d) / "target/landing-logs"
            logs.mkdir(parents=True)
            log = logs / "clippy-20261013T000000.000000Z-1.log"
            log.write_text("x\n")
            procs = [{"pid": 7, "cmd": "landing_gate.py", "repo": Path(d)}]
            rows = [{"checks": [{"label": "clippy", "duration_s": 100.0}]}] * 3
            now = log.stat().st_mtime + 30
            lines = status.gate_block(procs, now=now, timings=rows, started={log: now - 40})
            self.assertEqual(len(lines), 1)
            self.assertIn("pid 7", lines[0])
            self.assertIn("clippy", lines[0])
            self.assertIn("eta 60s", lines[0])

    def test_failing_source_prints_one_line(self):
        def boom(cmd, t):
            raise RuntimeError("nope")
        lines = status.guarded("gpu", lambda: status.gpu_block(boom), timeout=2)
        self.assertEqual(lines, ["gpu: unavailable (nope)"])

    def test_hanging_source_times_out(self):
        t0 = time.time()
        release = threading.Event()
        self.addCleanup(release.set)  # frees the abandoned worker so the interpreter exits at once
        lines = status.guarded("lanes", lambda: release.wait(30) or [], timeout=0.2)
        self.assertLess(time.time() - t0, 2)
        self.assertEqual(lines, ["lanes: timed out after 0.2s"])


class EtaTests(unittest.TestCase):
    def test_median_per_leg_ignores_null_and_zero(self):
        rows = [{"checks": [{"label": "tests/a", "duration_s": 10.0}, {"label": "tests", "duration_s": None}]},
                {"checks": [{"label": "tests", "duration_s": 30.0}]},
                {"checks": [{"label": "tests", "duration_s": 0}]},
                {"checks": [{"label": "tests", "duration_s": 20.0}]}]
        self.assertEqual(status.median_leg_seconds(rows)["tests"], 25.0)

    def test_eta_is_median_minus_elapsed_floored(self):
        med = {"clippy": 100.0}
        self.assertEqual(status.eta_seconds(med, "clippy", 40), 60)
        self.assertEqual(status.eta_seconds(med, "clippy", 250), 0)
        self.assertIsNone(status.eta_seconds(med, "unknown", 5))

    def test_read_timings_tolerates_bad_lines(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "t.jsonl"
            p.write_text(json.dumps({"checks": []}) + "\nnot json\n\n")
            self.assertEqual(status.read_timings(p), [{"checks": []}])


if __name__ == "__main__":
    unittest.main()
