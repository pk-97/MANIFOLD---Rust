#!/usr/bin/env python3
"""Focused tests for the gpu-proofs cargo command builder."""

import contextlib
import io
import sys
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

    def test_timings_collected_during_run(self):
        process = FakeProcess()
        process.stdout = iter(["     Running a (b)\n", "test x ... ok\n"])
        timings = []
        with patch.object(gate.subprocess, "Popen", return_value=process):
            with patch.object(gate.time, "monotonic", side_effect=[1.0, 4.0]):
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

    def run_main(self, argv, repo_changed=None):
        calls = []

        def fake_run_gate(manifest, filters, skips, targets, full, lib, timings):
            calls.append(dict(filters=filters, skips=skips, targets=targets, full=full, lib=lib))
            return 0, ""
        out = io.StringIO()
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(sys, "argv", ["gpu_proofs_gate.py", *argv]))
            stack.enter_context(patch.object(gate, "run_gate", side_effect=fake_run_gate))
            stack.enter_context(patch.object(gate, "changed_paths", return_value=repo_changed or []))
            stack.enter_context(patch.object(gate.gpu_queue, "hold", return_value=contextlib.nullcontext()))
            stack.enter_context(contextlib.redirect_stdout(out))
            code = gate.main()
        return code, calls, out.getvalue()

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

    def test_all_keeps_nightly_only_tests_and_scoped_skips_them(self):
        import gpu_scope
        code, calls, _ = self.run_main(["--all"])
        self.assertEqual(calls[0]["skips"], [])
        self.assertTrue(calls[0]["full"])
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-renderer/src/node_graph/primitives/matter_fill.rs"])
        for t in gpu_scope.NIGHTLY_ONLY:
            self.assertIn(t, calls[0]["skips"])

    def test_explicit_filter_bypasses_scoping(self):
        code, calls, text = self.run_main(["--filter", "water_"], repo_changed=["docs/X.md"])
        self.assertEqual(calls[0]["filters"], ["water_"])
        self.assertIn("GPU-PROOFS MODE: explicit", text)

    def test_gltf_paths_add_a_separate_unbudgeted_glb_run(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-renderer/tests/glb_conformance.rs"])
        self.assertEqual([c["targets"] for c in calls], [["gpu_proofs"], ["glb_conformance"]])
        self.assertEqual(calls[1]["filters"], [])


if __name__ == "__main__":
    unittest.main()
