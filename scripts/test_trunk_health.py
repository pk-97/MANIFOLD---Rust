#!/usr/bin/env python3
"""Fake-only tests for nightly reservation deferral."""

import json
import os
import sys
import time
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
        # Initial admission + two info legs precede clippy; five CPU-side
        # legs precede the first GPU hold. All commands below are fakes.
        for allowed_checks, expected_runs in ((3, 4), (6, 7)):
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

    def test_flaky_records_file_one_bead_per_test_and_note_existing(self):
        flaky = [("a t::x", "nextest/a", "k" * 32), ("a t::x", "nextest/b", "j" * 32),
                 ("b t::y", "nextest/a", "k" * 32)]
        existing = [{"id": "BUG-1", "title": "flaky test: b t::y", "notes": ""}]
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(cmd)
            if cmd[1] == "list":
                return type("R", (), {"returncode": 0, "stdout": json.dumps(existing), "stderr": ""})()
            return type("R", (), {"returncode": 0, "stdout": "", "stderr": ""})()

        with patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.gate_passes, "flaky_records", return_value=flaky), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_run), \
                contextlib.redirect_stdout(io.StringIO()):
            trunk_health.file_flaky_beads("abc")
        creates = [c for c in calls if c[1] == "create"]
        updates = [c for c in calls if c[1] == "update"]
        self.assertEqual([c[2] for c in creates], ["flaky test: a t::x"])
        self.assertIn("nextest/b", creates[0][-1])
        self.assertEqual([c[2] for c in updates], ["BUG-1"])

    def test_reservation_message_contains_owner_reason_and_expiry(self):
        info = {"owner": "owner", "reason": "reason", "end_epoch": 1_800_000_000}
        line = trunk_health.reservation_message(info)
        self.assertIn("owner", line)
        self.assertIn("reason", line)
        self.assertIn("1800000000", line)


def fake_bd(open_beads, calls, git_log="abc123 suspect commit\n"):
    def run(cmd, **kwargs):
        calls.append(cmd)
        if cmd[1] == "list":
            out = json.dumps(open_beads)
        elif cmd[0] == "git":
            out = git_log
        else:
            out = ""
        return type("R", (), {"returncode": 0, "stdout": out, "stderr": ""})()
    return run


class LegListTests(unittest.TestCase):
    def test_leg_list_after_cuts(self):
        info, cpu, gpu = trunk_health.nightly_legs(bridge_due=True)
        joined = [" ".join(c) for c in info + cpu + gpu]
        self.assertEqual([" ".join(c) for c in info],
                         ["python3 scripts/stale_beads.py", "python3 scripts/landing_metrics.py --days 7"])
        self.assertFalse(any("ignored-test-guard" in c or "cargo deny" in c for c in joined))
        self.assertTrue(any(c.startswith("cargo clippy --workspace") for c in joined))
        self.assertTrue(any("feature_matrix.py" in c for c in joined))
        self.assertTrue(any("gpu_proofs_gate.py --all" in c for c in joined))
        self.assertEqual(sum("rt_noise_gate.py" in c for c in joined), 2)
        self.assertTrue(any("bridge_probe_gate.py" in c for c in joined))
        self.assertEqual(len(cpu), 3)

    def test_bridge_probe_runs_weekly(self):
        _, _, gpu = trunk_health.nightly_legs(bridge_due=False)
        self.assertFalse(any("bridge_probe_gate.py" in " ".join(c) for c in gpu))
        with tempfile.TemporaryDirectory() as d:
            self.assertTrue(trunk_health.bridge_probe_due(Path(d)))
            log = Path(d, "2026-10-10.log")
            log.write_text(f"{trunk_health.BRIDGE_MARKER}\n[PASS] (300s)\n")
            self.assertFalse(trunk_health.bridge_probe_due(Path(d)))
            old = time.time() - 8 * 86400
            os.utime(log, (old, old))
            self.assertTrue(trunk_health.bridge_probe_due(Path(d)))
            Path(d, "2026-10-12.log").write_text("no probe tonight\n")
            self.assertTrue(trunk_health.bridge_probe_due(Path(d)))


NEXTEST_RED = """
        PASS [   0.010s] manifold-core tests::fine
        FAIL [   0.020s] manifold-core tests::broken
        FAIL [   0.020s] manifold-editing undo::cap
"""


class RoutingTests(unittest.TestCase):
    def test_failed_anywhere_tonight_never_closes(self):
        nextest = "        FAIL [   0.1s] (1/2) manifold-core tests::x\n"
        cargo = ("$ cargo test -p manifold-core --features gpu-proofs --no-fail-fast\n"
                 "     Running unittests src/lib.rs (target/debug/deps/manifold_core-ab12cd)\n"
                 "test tests::x ... ok\n")
        results = [trunk_health.classify_leg("cargo nextest run", 100, nextest, ""),
                   trunk_health.classify_leg("python3 scripts/gpu_proofs_gate.py --all", 0, cargo, "")]
        calls = []
        open_beads = [{"id": "BUG-x", "title": "nightly red: manifold-core tests::x", "description": ""}]
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd(open_beads, calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            trunk_health.route_nightly("sha", results, ["python3 scripts/gpu_proofs_gate.py --all"],
                                       Path(d, "g.json"))
        self.assertFalse([c for c in calls if c[1] == "close"])
        self.assertTrue([c for c in calls if c[1] == "update" and c[2] == "BUG-x"])

    def test_same_test_in_two_packages_gets_two_beads_with_real_crates(self):
        out = ("$ cargo test -p manifold-nodes --features gpu-proofs --no-fail-fast\n"
               "     Running tests/gpu_proofs.rs (target/debug/deps/gpu_proofs-ab12)\n"
               "test a::b ... FAILED\n"
               "$ cargo test -p manifold-gpu --features gpu-proofs --no-fail-fast\n"
               "     Running tests/gpu_proofs.rs (target/debug/deps/gpu_proofs-cd34)\n"
               "test a::b ... FAILED\n")
        result = trunk_health.classify_leg("python3 scripts/gpu_proofs_gate.py --all", 1, out, "")
        calls = []
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            trunk_health.route_nightly("sha", [result], [], Path(d, "g.json"))
        creates = [c for c in calls if c[1] == "create"]
        self.assertEqual(len(creates), 2)
        labels = [c[c.index("-l") + 1] for c in creates]
        self.assertTrue(any("manifold-nodes" in l for l in labels))
        self.assertTrue(any("manifold-gpu" in l for l in labels))
        self.assertFalse(any("gpu-proofs" in l for l in labels))

    def test_non_dict_last_green_is_treated_as_empty(self):
        result = trunk_health.classify_leg("cargo nextest run", 100, NEXTEST_RED, "")
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], [])), \
                contextlib.redirect_stdout(io.StringIO()):
            state = Path(d, "g.json")
            state.write_text("[1, 2]")
            trunk_health.route_nightly("sha", [result], [], state)
            self.assertIsInstance(json.loads(state.read_text()), dict)

    def test_parse_nextest_and_cargo_test_output(self):
        failed, passed = trunk_health.parse_test_results(NEXTEST_RED)
        self.assertEqual(failed, [("manifold-core", "tests::broken"), ("manifold-editing", "undo::cap")])
        self.assertIn(("manifold-core", "tests::fine"), passed)
        cargo = ("$ cargo test -p manifold-gpu --features gpu-proofs --no-fail-fast\n"
                 "     Running tests/gpu_proofs.rs (target/debug/deps/gpu_proofs-ab12)\n"
                 "test a::b ... FAILED\n"
                 "     Running unittests src/lib.rs (target/debug/deps/manifold_nodes-cd34)\n"
                 "test c::d ... ok\n")
        failed, passed = trunk_health.parse_test_results(cargo)
        self.assertEqual(failed, [("manifold-gpu", "gpu_proofs/a::b")])
        self.assertIn(("manifold-nodes", "c::d"), passed)

    def test_failing_tests_route_to_one_bead_each_with_suspects(self):
        result = trunk_health.classify_leg("cargo nextest run", 100, NEXTEST_RED, "")
        self.assertEqual(result["kind"], "tests")
        calls = []
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            state = Path(d, "last_green.json")
            state.write_text(json.dumps({"manifold-core": "000green"}))
            trunk_health.route_nightly("sha1", [result], [], state)
            # Failing crates keep their last green sha.
            self.assertEqual(json.loads(state.read_text())["manifold-core"], "000green")
        creates = [c for c in calls if c[1] == "create"]
        self.assertEqual([c[2] for c in creates],
                         ["nightly red: manifold-core tests::broken", "nightly red: manifold-editing undo::cap"])
        desc = creates[0][creates[0].index("-d") + 1]
        self.assertIn("Summary", desc)
        self.assertIn("000green", desc)
        self.assertIn("abc123 suspect commit", desc)
        labels = creates[0][creates[0].index("-l") + 1]
        self.assertIn("nightly", labels)
        self.assertIn("manifold-core", labels)
        git = [c for c in calls if c[0] == "git"]
        self.assertTrue(any("000green..sha1" in c for c in git))

    def test_more_than_ten_failures_fold_into_one_leg_bead(self):
        out = "\n".join(f"        FAIL [ 0.1s] manifold-core t::n{i}" for i in range(11))
        result = trunk_health.classify_leg("cargo nextest run", 100, out, "")
        calls = []
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            trunk_health.route_nightly("sha1", [result], [], Path(d, "g.json"))
        creates = [c for c in calls if c[1] == "create"]
        self.assertEqual(len(creates), 1)
        self.assertEqual(creates[0][2], "nightly red leg: cargo nextest run")
        desc = creates[0][creates[0].index("-d") + 1]
        self.assertIn("t::n10", desc)

    def test_bead_closes_when_its_test_passes_later(self):
        result = trunk_health.classify_leg("cargo nextest run", 0, NEXTEST_RED.replace("FAIL", "PASS"), "")
        calls = []
        open_beads = [{"id": "BUG-a", "title": "nightly red: manifold-core tests::broken", "description": ""},
                      {"id": "BUG-b", "title": "nightly red: manifold-x other::t", "description": ""}]
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd(open_beads, calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            state = Path(d, "g.json")
            trunk_health.route_nightly("sha2", [result], ["cargo nextest run"], state)
            self.assertEqual(json.loads(state.read_text())["manifold-core"], "sha2")
        closes = [c for c in calls if c[1] == "close"]
        self.assertEqual([c[2] for c in closes], ["BUG-a"])

    def test_crash_timeout_and_refusal_share_one_infra_bead(self):
        results = [trunk_health.infra_result("cargo nextest run", "timed out"),
                   trunk_health.classify_leg("python3 scripts/bridge_probe_gate.py", 2, "EXIT 2: GPU busy", ""),
                   trunk_health.classify_leg("cargo clippy", -11, "", "")]
        self.assertTrue(all(r["kind"] == "infra" for r in results))
        calls = []
        with tempfile.TemporaryDirectory() as d, \
                patch.object(trunk_health, "BD", "bd"), \
                patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], calls)), \
                contextlib.redirect_stdout(io.StringIO()):
            trunk_health.route_nightly("sha1", results, [], Path(d, "g.json"))
        creates = [c for c in calls if c[1] == "create"]
        self.assertEqual([c[2] for c in creates], [trunk_health.INFRA_TITLE])
        desc = creates[0][creates[0].index("-d") + 1]
        self.assertIn("timed out", desc)
        self.assertIn("bridge_probe_gate.py", desc)

    def test_info_legs_never_file(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            def fake_run(cmd, **kwargs):
                if "stale_beads.py" in " ".join(cmd) or "landing_metrics.py" in " ".join(cmd):
                    return 1, "red metrics", "", 0
                return 0, "tip", "", 0
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["trunk_health.py"]))
            stack.enter_context(patch.object(trunk_health, "LOG_DIR", Path(d)))
            stack.enter_context(patch.object(trunk_health, "missing_tools", return_value=[]))
            stack.enter_context(patch.object(trunk_health, "cap_main_target", return_value=""))
            stack.enter_context(patch.object(trunk_health.gpu_queue, "reservation", return_value={}))
            stack.enter_context(patch.object(trunk_health.gpu_queue, "hold"))
            stack.enter_context(patch.object(trunk_health, "run_cmd", side_effect=fake_run))
            stack.enter_context(patch.object(trunk_health, "file_flaky_beads"))
            calls = []
            stack.enter_context(patch.object(trunk_health, "BD", "bd"))
            stack.enter_context(patch.object(trunk_health.subprocess, "run", side_effect=fake_bd([], calls)))
            self.assertEqual(trunk_health.main(), 0)
            self.assertFalse([c for c in calls if c[1] == "create"])
            self.assertIn("red metrics", next(Path(d).glob("*.log")).read_text())


class InterpreterGuardTests(unittest.TestCase):
    def test_modern_python_does_not_reexec(self):
        execs = []
        trunk_health.ensure_modern_python((3, 11, 0), ["/x"], execv=lambda *a: execs.append(a))
        self.assertEqual(execs, [])

    def test_old_python_reexecs_under_first_modern_candidate(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d, "a"), Path(d, "b")
            a.mkdir()
            b.mkdir()
            for dirp in (a, b):
                p = dirp / "python3"
                p.write_text("#!/bin/sh\necho 3.12\n")
                p.chmod(0o755)
            execs = []
            with patch.object(trunk_health, "python_version_of", return_value=(3, 12)):
                trunk_health.ensure_modern_python(
                    (3, 9, 6), [str(a), str(b)], argv=["t.py", "--dry-run"],
                    execv=lambda *args: execs.append(args))
            self.assertEqual(execs[0][0], str(a / "python3"))
            self.assertEqual(execs[0][1], [str(a / "python3"), "t.py", "--dry-run"])

    def test_no_modern_python_aborts_loudly(self):
        aborts = []
        with tempfile.TemporaryDirectory() as d:
            trunk_health.ensure_modern_python(
                (3, 9, 6), [d], argv=["t.py"], execv=lambda *a: self.fail("exec"),
                abort=lambda msg: aborts.append(msg))
        self.assertEqual(len(aborts), 1)
        self.assertIn("3.11", aborts[0])

    def test_abort_logs_files_bead_notifies_and_exits_2(self):
        with tempfile.TemporaryDirectory() as d:
            ran = []
            with patch.object(trunk_health, "LOG_DIR", Path(d)), \
                 patch.object(trunk_health.subprocess, "run", side_effect=lambda c, **k: ran.append(c)):
                with self.assertRaises(SystemExit) as cm:
                    trunk_health.abort_no_modern_python("no python")
            self.assertEqual(cm.exception.code, 2)
            logs = list(Path(d).glob("*.log"))
            self.assertIn("[ABORT]", logs[0].read_text())
            self.assertTrue(any(c[1] == "create" and "1" in c for c in ran))
            self.assertTrue(any(c[0] == "osascript" for c in ran))


_PROD_LAST_GREEN = trunk_health.LAST_GREEN


def _stat(path):
    with contextlib.suppress(OSError):
        st = path.stat()
        return st.st_mtime_ns, st.st_size
    return None


_PROD_BEFORE = _stat(_PROD_LAST_GREEN)
_SANDBOX = tempfile.TemporaryDirectory()
_PATCHES = [patch.object(trunk_health, "LOG_DIR", Path(_SANDBOX.name)),
            patch.object(trunk_health, "LAST_GREEN", Path(_SANDBOX.name, "last_green.json"))]


def setUpModule():
    # Every test runs with production state paths pointed at a sandbox.
    for p in _PATCHES:
        p.start()


def tearDownModule():
    for p in _PATCHES:
        p.stop()
    _SANDBOX.cleanup()


class ProductionPathTests(unittest.TestCase):
    def test_zz_production_last_green_untouched(self):
        # Named to sort last so it sees every earlier test's writes.
        self.assertNotEqual(trunk_health.LAST_GREEN, _PROD_LAST_GREEN)
        self.assertEqual(_stat(_PROD_LAST_GREEN), _PROD_BEFORE)


if __name__ == "__main__":
    unittest.main()
