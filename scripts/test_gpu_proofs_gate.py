#!/usr/bin/env python3
"""Focused tests for the gpu-proofs cargo command builder."""

import contextlib
import io
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import gpu_proofs_gate as gate


class FakeProcess:
    def __init__(self, returncode=0):
        self.stdout = iter(["cargo output\n"])
        self.returncode = returncode

    def wait(self):
        return self.returncode


def run_gate(*, targets=None, lib=False, full_suite=False, filters=None, skips=None, returncode=0):
    process = FakeProcess(returncode)
    with patch.object(gate.subprocess, "Popen", return_value=process) as popen:
        result = gate.run_gate(
            Path("/tmp/Cargo.toml"), filters or [], skips or [], targets, full_suite, lib
        )
    return popen.call_args.args[0], result


class GpuProofsGateTests(unittest.TestCase):
    def test_default_targets_only_gpu_proofs(self):
        command, _ = run_gate()

        self.assertEqual(command[command.index("--test") + 1], "gpu_proofs")
        self.assertIn("--no-fail-fast", command)
        self.assertEqual(command[command.index("--") + 1 :], ["--test-threads=1"])

    def test_repeatable_named_targets(self):
        command, _ = run_gate(targets=["alpha", "beta"])

        self.assertEqual(
            [command[i + 1] for i, item in enumerate(command) if item == "--test"],
            ["alpha", "beta"],
        )
        self.assertNotIn("gpu_proofs", command)
        self.assertIn("--no-fail-fast", command)

    def test_full_suite_has_no_target_and_no_fail_fast(self):
        command, _ = run_gate(full_suite=True)

        self.assertNotIn("--test", command)
        self.assertIn("--no-fail-fast", command)

    def test_filters_and_skips_follow_libtest_delimiter(self):
        command, (returncode, output) = run_gate(
            filters=["first", "second"], skips=["slow", "flaky"], returncode=7
        )

        delimiter = command.index("--")
        self.assertEqual(
            command[delimiter + 1 :],
            [
                "--test-threads=1",
                "first",
                "second",
                "--skip",
                "slow",
                "--skip",
                "flaky",
            ],
        )
        self.assertEqual(returncode, 7)
        self.assertEqual(output, "cargo output\n")

    def test_full_suite_and_named_target_are_mutually_exclusive(self):
        with self.assertRaises(ValueError):
            run_gate(targets=["alpha"], full_suite=True)

    def test_cli_rejects_conflicting_scope_flags(self):
        stderr = io.StringIO()
        with patch.object(sys, "argv", ["gpu_proofs_gate.py", "--test", "alpha", "--full-suite"]):
            with contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit) as error:
                    gate.main()

        self.assertEqual(error.exception.code, 2)
        self.assertIn("not allowed with argument", stderr.getvalue())

    def test_lib_flag_adds_lib_target(self):
        command, _ = run_gate(lib=True, targets=["gpu_proofs"])
        self.assertIn("--lib", command)
        self.assertEqual(command[command.index("--test") + 1], "gpu_proofs")

    def test_timing_is_gap_between_finished_test_lines(self):
        timings, state = [], {"t": None, "bin": ""}
        feed = [
            (0.0, "     Running tests/gpu_proofs/main.rs (target/debug/deps/gpu_proofs-ab)\n"),
            (3.0, "test a::one ... ok\n"),
            (3.5, "thread 'x' panicked at foo\n"),
            (13.5, "test a::two ... FAILED\n"),
        ]
        for now, line in feed:
            gate.record_timing(line, now, state, timings)
        self.assertEqual([(n, round(s, 1)) for n, s, _ in timings], [("a::one", 3.0), ("a::two", 10.5)])

    def test_result_split_by_native_output_is_charged_to_its_own_test(self):
        timings, state = [], {"t": None, "bin": ""}
        feed = [
            (0.0, "     Running tests/gpu_proofs/main.rs (target/debug/deps/gpu_proofs-ab)\n"),
            (1.0, "test a::slow ... ------------\n"),
            (2.0, "Fluid Engine Version 1.8.8\n"),
            (200.0, "ok\n"),
            (201.0, "test a::fast ... ok\n"),
        ]
        for now, line in feed:
            gate.record_timing(line, now, state, timings)
        self.assertEqual([(n, round(s, 1)) for n, s, _ in timings], [("a::slow", 200.0), ("a::fast", 1.0)])

    def test_timings_collected_during_run(self):
        process = FakeProcess()
        process.stdout = iter(["     Running a (b)\n", "test x ... ok\n"])
        timings = []
        with patch.object(gate.subprocess, "Popen", return_value=process):
            ticks = iter([1.0, 4.0] + [4.0] * 50)
            with patch.object(gate.time, "monotonic", side_effect=lambda: next(ticks)):
                with contextlib.redirect_stdout(io.StringIO()):
                    gate.run_gate(Path("/tmp/Cargo.toml"), [], [], None, False, False, timings)
        self.assertEqual(timings, [("x", 3.0, "a")])

    def summary(self, timings, budget, exit_code=0):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", exit_code, timings, budget)
        return code, out.getvalue()

    def test_over_budget_fails_and_names_slowest(self):
        timings = [("fast", 10.0, "b", True), ("slow_one", 200.0, "b", True), ("slow_two", 120.0, "b", True)]
        code, text = self.summary(timings, 300)
        self.assertEqual(code, 3)
        self.assertIn("over time budget: 330s > 300s", text)
        self.assertLess(text.index("slow_one"), text.index("slow_two"))
        self.assertLess(text.index("Slowest tests"), text.index("GPU-PROOFS GATE: FAIL"))

    def test_under_budget_passes_and_unbudgeted_runs_are_exempt(self):
        timings = [("a", 100.0, "b", True), ("glb_sweep", 957.0, "glb", False)]
        code, text = self.summary(timings, 300)
        self.assertEqual(code, 0)
        self.assertIn("GPU-PROOFS GATE: PASS", text)

    def test_no_budget_never_fails_on_time(self):
        code, _ = self.summary([("a", 9999.0, "b", True)], None)
        self.assertEqual(code, 0)

    def run_main(self, argv, repo_changed=None, build_exit=0):
        calls = []
        self.events = events = []

        def fake_run_gate(manifest, filters, skips, targets, full, lib, timings, hung=None,
                          hang_floor=None):
            calls.append(dict(filters=filters, skips=skips, targets=targets, full=full, lib=lib))
            events.append(("run", gate.cargo_test_cmd(manifest, targets, full, lib)))
            return 0, ""

        def fake_build(cmd, **kwargs):
            events.append(("build", cmd))
            return subprocess.CompletedProcess(cmd, build_exit)

        @contextlib.contextmanager
        def recording_hold(label, **kwargs):
            events.append(("hold-enter", label))
            try:
                yield
            finally:
                events.append(("hold-exit", label))
        out = io.StringIO()
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(sys, "argv", ["gpu_proofs_gate.py", *argv]))
            stack.enter_context(patch.object(gate, "run_gate", side_effect=fake_run_gate))
            stack.enter_context(patch.object(gate.subprocess, "run", side_effect=fake_build))
            stack.enter_context(patch.object(gate, "changed_paths", return_value=repo_changed or []))
            stack.enter_context(patch.object(gate.gpu_queue, "hold", side_effect=recording_hold))
            stack.enter_context(contextlib.redirect_stdout(out))
            code = gate.main()
        return code, calls, out.getvalue()

    def test_test_binaries_build_before_the_hold_with_the_run_arguments(self):
        code, _, _ = self.run_main(
            [], repo_changed=["crates/manifold-renderer/src/node_graph/primitives/invert.rs"])
        self.assertEqual(code, 0)
        kinds = [kind for kind, _ in self.events]
        self.assertEqual(kinds, ["build", "hold-enter", "run", "hold-exit"])
        (_, build), (_, run) = self.events[0], self.events[2]
        self.assertEqual(build, run + ["--no-run"])

    def test_each_distinct_run_builds_once_before_any_test(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-renderer/tests/glb_conformance.rs"])
        self.assertEqual(code, 0)
        self.assertGreater(len(calls), 1)
        kinds = [kind for kind, _ in self.events]
        first_hold = kinds.index("hold-enter")
        builds = [cmd for kind, cmd in self.events if kind == "build"]
        self.assertTrue(all(k == "build" for k in kinds[:first_hold]))
        self.assertNotIn("build", kinds[first_hold:])
        self.assertEqual(len(builds), len({tuple(b) for b in builds}))
        runs = {tuple(cmd + ["--no-run"]) for kind, cmd in self.events if kind == "run"}
        self.assertEqual(runs, {tuple(b) for b in builds})

    def test_build_failure_takes_no_lock_and_runs_nothing(self):
        code, calls, text = self.run_main(["--all"], build_exit=101)
        self.assertEqual(code, 101)
        self.assertEqual(calls, [])
        self.assertEqual([kind for kind, _ in self.events], ["build"])
        self.assertIn("GPU-PROOFS GATE: FAIL (test build failed, exit 101; no GPU lock taken)", text)

    def test_build_only_compiles_and_stops_without_the_lock(self):
        code, calls, text = self.run_main(["--build-only", "--all"])
        self.assertEqual(code, 0)
        self.assertEqual(calls, [])
        self.assertEqual([kind for kind, _ in self.events], ["build"])
        self.assertIn("GPU-PROOFS GATE: BUILT", text)

    def test_default_is_scoped_from_diff_and_prints_mode(self):
        p = "crates/manifold-renderer/src/node_graph/primitives/invert.rs"
        code, calls, text = self.run_main([], repo_changed=[p])
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 1)
        self.assertTrue(calls[0]["lib"])
        self.assertFalse(calls[0]["full"])
        self.assertIn("node_graph::primitives::invert::", calls[0]["filters"])
        self.assertIn("GPU-PROOFS MODE: scoped", text)

    def test_default_with_no_gpu_paths_runs_nothing(self):
        code, calls, text = self.run_main([], repo_changed=["docs/X.md"])
        self.assertEqual((code, calls), (0, []))
        self.assertIn("nothing to run", text)

    def test_unmapped_path_fails_without_running(self):
        code, calls, text = self.run_main(
            [], repo_changed=["crates/manifold-renderer/src/node_graph/x.bin"])
        self.assertEqual(code, 2)
        self.assertEqual(calls, [])
        self.assertIn("node_graph/x.bin", text)
        self.assertIn("no run-everything fallback", text)

    def test_all_flag_runs_full_suite_and_prints_mode(self):
        code, calls, text = self.run_main(["--all"])
        self.assertTrue(calls[0]["full"])
        self.assertIn("GPU-PROOFS MODE: all", text)

    def test_all_keeps_slow_tests_and_scoped_skips_them(self):
        import json, tempfile
        import gpu_scope
        d = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        (d / "t.json").write_text(json.dumps({"tests": {"m::slow": 90.0, "m::fast": 5.0}}))
        with patch.object(gpu_scope, "TIMES_PATH", d / "t.json"):
            code, calls, _ = self.run_main(["--all"])
            self.assertEqual(calls[0]["skips"], [])
            self.assertTrue(calls[0]["full"])
            code, calls, _ = self.run_main(["--filter", "m::slow"])
            self.assertEqual(calls[0]["skips"], [])
            code, calls, _ = self.run_main(
                [], repo_changed=["crates/manifold-renderer/src/node_graph/primitives/matter_fill.rs"])
            self.assertIn("m::slow", calls[0]["skips"])
            self.assertNotIn("m::fast", calls[0]["skips"])

    def test_record_times_writes_json_and_prints_diff_without_touching_repo_file(self):
        import json, tempfile
        import gpu_scope
        d = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        committed = d / "committed.json"
        committed.write_text(json.dumps({"tests": {"m::was_slow": 90.0}}))
        before = committed.read_text()
        out = d / "out.json"
        with patch.object(gpu_scope, "TIMES_PATH", committed):
            text = gate.write_times_json(out, [("m::was_slow", 10.0, "b", True),
                                               ("m::new_slow", 70.0, "b", True)])
        data = json.loads(out.read_text())
        self.assertEqual(data["tests"], {"m::new_slow": 70.0, "m::was_slow": 10.0})
        self.assertIn("now fast: m::was_slow", text)
        self.assertIn("new SLOW: m::new_slow", text)
        self.assertEqual(committed.read_text(), before)

    def test_over_budget_message_says_how_to_fix(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", 0, [("slow", 400.0, "b", True)], 300)
        self.assertEqual(code, 3)
        self.assertIn("--record-times", out.getvalue())
        self.assertIn("shorten", out.getvalue())

    def test_explicit_filter_bypasses_scoping(self):
        code, calls, text = self.run_main(["--filter", "water_"], repo_changed=["docs/X.md"])
        self.assertEqual(calls[0]["filters"], ["water_"])
        self.assertIn("GPU-PROOFS MODE: explicit", text)

    def test_gltf_paths_add_a_separate_unbudgeted_glb_run(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-renderer/tests/glb_conformance.rs"])
        self.assertEqual([c["targets"] for c in calls], [["gpu_proofs"], ["glb_conformance"]])
        self.assertEqual(calls[1]["filters"], [])


class WatchdogTests(unittest.TestCase):
    TIMES = {"m::known": 100.0}

    def dog(self, floor=None):
        return gate.Watchdog(self.TIMES, floor)

    def test_allowance_is_floor_five_times_record_or_no_record_default(self):
        d = self.dog()
        self.assertEqual(d.allowance("m::known"), 500.0)
        self.assertEqual(d.allowance("m::unknown"), 300.0)
        d.times = {"m::tiny": 2.0}
        self.assertEqual(d.allowance("m::tiny"), 120.0)

    def test_floor_override_replaces_floor_and_no_record_default(self):
        d = self.dog(floor=10.0)
        self.assertEqual(d.allowance("m::unknown"), 10.0)
        d.times = {"m::tiny": 1.0, "m::big": 50.0}
        self.assertEqual(d.allowance("m::tiny"), 10.0)
        self.assertEqual(d.allowance("m::big"), 250.0)

    def test_unfinished_tail_starts_the_clock_and_hang_fires(self):
        d = self.dog()
        d.feed_line("     Running a (b)\n", 0.0)
        d.feed_partial("test m::unknown ... ", 10.0)
        self.assertIsNone(d.check(300.0))
        name, waited, allowance = d.check(311.0)
        self.assertEqual((name, round(waited), allowance), ("m::unknown", 301, 300.0))

    def test_result_line_stops_the_clock(self):
        for result in ("test m::a ... ok\n", "test m::a ... FAILED\n", "test m::a ... ignored\n"):
            d = self.dog()
            d.feed_partial("test m::a ... ", 0.0)
            d.feed_line(result, 5.0)
            self.assertIsNone(d.check(9999.0), result)

    def test_result_split_by_native_output_stops_the_clock(self):
        d = self.dog()
        d.feed_line("test m::a ... ------------\n", 0.0)
        d.feed_line("Fluid Engine Version 1.8.8\n", 1.0)
        self.assertIsNotNone(d.check(400.0))
        d.feed_line("ok\n", 401.0)
        self.assertIsNone(d.check(9999.0))

    def test_partial_then_full_line_does_not_restart_the_clock(self):
        d = self.dog()
        d.feed_partial("test m::a ... ", 0.0)
        d.feed_line("test m::a ... ------\n", 50.0)
        self.assertEqual(d.check(301.0)[0], "m::a")

    def test_heartbeat_every_minute_names_the_test(self):
        d = self.dog()
        d.feed_partial("test m::known ... ", 0.0)
        self.assertIsNone(d.heartbeat(59.0))
        beat = d.heartbeat(61.0)
        self.assertIn("m::known", beat)
        self.assertIn("61s", beat)
        self.assertIsNone(d.heartbeat(100.0))
        self.assertIsNotNone(d.heartbeat(122.0))

    def test_idle_between_tests_never_fires(self):
        d = self.dog()
        d.feed_line("test m::a ... ok\n", 0.0)
        self.assertIsNone(d.check(9999.0))
        self.assertIsNone(d.heartbeat(9999.0))

    def test_hung_run_is_killed_and_reported(self):
        real_popen = subprocess.Popen

        def sh_popen(cmd, **kwargs):
            return real_popen(["/bin/sh", "-c", "printf 'test m::stuck ... '; sleep 60"], **kwargs)

        hung = []
        out = io.StringIO()
        started = time.monotonic()
        with patch.object(gate.subprocess, "Popen", side_effect=sh_popen):
            with contextlib.redirect_stdout(out):
                code, _ = gate.run_gate(Path("/tmp/Cargo.toml"), [], [], hung=hung, hang_floor=1.0)
        self.assertLess(time.monotonic() - started, 30)
        self.assertNotEqual(code, 0)
        self.assertEqual([n for n, _ in hung], ["m::stuck"])
        self.assertIn("GPU-PROOFS GATE: HUNG m::stuck after", out.getvalue())

    def test_summary_fails_loudly_on_hang(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", 0, [], None, [("m::stuck", 130.4)])
        self.assertEqual(code, 4)
        self.assertIn("GPU-PROOFS GATE: HUNG m::stuck after 130s", out.getvalue())


if __name__ == "__main__":
    unittest.main()
