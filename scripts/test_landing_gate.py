#!/usr/bin/env python3
"""Exercise gate sequencing and delivery without Cargo, rendering, or git writes."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

import landing_gate
import land_branch
import trunk_health


def process_alive(pid):
    """True while `pid` runs; a reaped or zombie process counts as gone."""
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                           capture_output=True, text=True).stdout.strip()
    return bool(state) and not state.startswith("Z")


class LandingTests(unittest.TestCase):
    checks = ["tooling", "design-status", "docs-index", "deny", "ignored-tests",
              "clippy", "flow-gate", "tests", "gpu-proofs"]

    def exercise(self, failed=None, extra=(), stale_docs=False, packages=True, head="head", paths=None):
        called, commands = [], []
        self.events = events = []
        paths = paths or ["crates/manifold-gpu/src/metal/device.rs"]
        labels = {
            "fake-tool-test.py": "tooling", "design_status_check.py": "design-status",
            "gen_docs_index.py": "docs-index", "ignored-test-guard.py": "ignored-tests",
            "run_ui_flows.py": "flow-gate", "gpu_proofs_gate.py": "gpu-proofs",
        }

        @contextlib.contextmanager
        def recording_hold(label, **kwargs):
            events.append(f"hold-enter:{label}")
            try:
                yield
            finally:
                events.append("hold-exit")

        def run(cmd, cwd, timeout, live_log=None):
            commands.append(cmd)
            if cmd[0] == "git":
                if cmd[1] == "merge-base":
                    out = "base"
                elif cmd[1] == "diff":
                    if "docs/README.md" in cmd:
                        out = "docs/README.md" if stale_docs else ""
                    elif "docs/" in cmd:
                        out = "docs/new.md"
                    else:
                        out = ("\0" if "-z" in cmd else "\n").join(paths)
                elif cmd[1:] == ["rev-parse", "HEAD"]:
                    out = head
                elif cmd[1] == "rev-parse":
                    out = "codex/test"
                else:
                    out = ""
                return 0, out, "", 0.01
            label = ({"nextest": "tests"}.get(cmd[1], cmd[1]) if cmd[0] == "cargo"
                     else labels[Path(cmd[1]).name])
            called.append(label)
            events.append(label)
            return (1 if label == failed else 0), f"output for {label}\n", "", 0.01

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            root = Path(d)
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["landing_gate.py", "--repo", d, *extra]))
            stack.enter_context(patch.object(landing_gate, "MAIN_CHECKOUT", root))
            stack.enter_context(patch.object(landing_gate, "run_cmd", side_effect=run))
            # A tooling self-test must never wait on the machine-wide GPU lock.
            stack.enter_context(patch.object(landing_gate.gpu_queue, "hold",
                                             side_effect=recording_hold))
            stack.enter_context(patch.object(landing_gate, "get_touched_packages",
                                            return_value=["manifold-gpu"] if packages else []))
            deps = stack.enter_context(patch.object(landing_gate, "reverse_deps", return_value=[]))
            stack.enter_context(patch("codex_checks.tooling_checks", return_value=[{
                "name": "tooling", "argv": ["python3", "fake-tool-test.py"],
            }]))
            code = landing_gate.main()
            timing_log = root / ".claude/orchestration/landing-gate-timings.jsonl"
            timings = json.loads(timing_log.read_text()) if timing_log.exists() else None
            logs = [p.read_text() for p in (root / "target/landing-logs").glob("*.log")]
            return code, called, timings, commands, logs, output.getvalue(), deps.call_count

    def test_every_failure_stops_later_checks_and_retains_evidence(self):
        for index, failed in enumerate(self.checks):
            with self.subTest(failed=failed):
                code, called, timings, commands, logs, output, deps = self.exercise(failed)
                self.assertEqual(code, 1)
                self.assertEqual(called, self.checks[:index + 1])
                self.assertEqual(timings["failed"], 1)
                self.assertEqual(timings["checks"][-1]["status"], "FAIL")
                self.assertTrue(any(f"output for {failed}" in log for log in logs))
                self.assertIn(f"[RUN] {failed}", output)
                self.assertNotIn(["git", "log", "base..HEAD", "--format=%B"], commands)
                if index < 5:
                    self.assertEqual(deps, 0)

    def test_stale_docs_stop_before_cargo_or_rendering(self):
        code, called, timings, *_ = self.exercise(stale_docs=True)
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks[:3])
        self.assertEqual(timings["checks"][-1]["label"], "docs-index")

    def test_success_runs_all_required_checks_with_explicit_gpu_binary(self):
        code, called, timings, commands, _, output, _ = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(called, self.checks)
        self.assertEqual(timings["failed"], 0)
        self.assertIn(["python3", "scripts/gpu_proofs_gate.py", "--base", "origin/main",
                       "--budget", "300"], commands)
        self.assertTrue(all("--all" not in c and "--full-suite" not in c for c in commands))
        self.assertIn("[gpu-proofs] mode: scoped", output)
        self.assertIn("manifold-gpu core", output)

    def test_tests_and_gpu_proofs_legs_run_inside_one_hold(self):
        code, *_ = self.exercise()
        self.assertEqual(code, 0)
        events = self.events
        self.assertEqual([e for e in events if e.startswith("hold")],
                         ["hold-enter:landing_gate tests+gpu-proofs", "hold-exit"])
        enter, leave = events.index("hold-enter:landing_gate tests+gpu-proofs"), events.index("hold-exit")
        self.assertLess(enter, events.index("tests"))
        self.assertLess(events.index("gpu-proofs"), leave)
        # Cheap prerequisites and clippy do not hold the GPU.
        self.assertLess(events.index("clippy"), enter)

    def test_tests_failure_still_releases_the_hold(self):
        self.exercise("tests")
        self.assertEqual(self.events[-1], "hold-exit")

    def test_unmapped_gpu_path_fails_gate_naming_path(self):
        code, called, _, _, _, output, _ = self.exercise(
            paths=["crates/manifold-renderer/src/node_graph/orphan.bin"])
        self.assertEqual(code, 1)
        self.assertNotIn("gpu-proofs", called)
        self.assertIn("node_graph/orphan.bin", output)
        self.assertIn("Add a mapping rule", output)

    def test_named_red_collection_does_not_skip_remaining_checks(self):
        code, called, timings, *_ = self.exercise("docs-index", extra=["--keep-going"])
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks)
        self.assertEqual(timings["failed"], 1)

    def test_no_rust_packages_does_not_load_cargo_metadata(self):
        *_, deps = self.exercise(packages=False)
        self.assertEqual(deps, 0)

    def test_head_equal_to_base_fails_before_any_check(self):
        # The main checkout sits on origin/main: a run there must not pass
        # on an all-skipped gate.
        code, called, timings, _, _, output, _ = self.exercise(head="base")
        self.assertEqual(code, 1)
        self.assertEqual(called, [])
        self.assertIsNone(timings)
        self.assertIn("[FAIL] HEAD == origin/main", output)
        self.assertIn("Pass --repo <worktree path>", output)

    def test_every_skip_names_its_reason(self):
        *_, output, _ = self.exercise(packages=False, extra=["--skip-gpu", "deferred"])
        self.assertIn("[SKIP] clippy (no touched packages)", output)
        self.assertIn("[SKIP] tests (no touched packages)", output)
        self.assertIn("[SKIP] gpu-proofs (skipped by flag: deferred)", output)
        self.assertIn("SKIP clippy (no touched packages)\n", output)

    def test_timeout_retains_partial_output_and_kills_grandchildren(self):
        # The shape of a hung flow gate: a script whose own child outlives the
        # timeout. Killing only the direct child orphaned the app, which kept
        # the GPU lock.
        with tempfile.TemporaryDirectory() as d:
            pidfile = Path(d) / "grandchild.pid"
            script = (
                "import subprocess, sys, time\n"
                "g = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])\n"
                f"open({str(pidfile)!r}, 'w').write(str(g.pid))\n"
                "print('before timeout', flush=True)\n"
                "print('detail', file=sys.stderr, flush=True)\n"
                "time.sleep(120)\n")
            live = Path(d) / "live.log"
            code, out, err, duration = landing_gate.run_cmd(
                [sys.executable, "-c", script], Path(d), 3, live_log=live)
            grandchild = int(pidfile.read_text())
            self.assertEqual(code, -1)
            self.assertEqual(out, "before timeout\n")
            self.assertIn("detail", err)
            self.assertIn("TIMEOUT", err)
            self.assertLess(duration, 60)
            self.assertIn("before timeout", live.read_text())
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and process_alive(grandchild):
                time.sleep(0.1)
            self.assertFalse(process_alive(grandchild), "grandchild survived the timeout")

    def test_live_log_holds_lines_before_the_command_exits(self):
        with tempfile.TemporaryDirectory() as d:
            live = Path(d) / "live.log"
            gate = Path(d) / "go"
            script = (
                "import os, sys, time\n"
                "print('first line', flush=True)\n"
                f"while not os.path.exists({str(gate)!r}): time.sleep(0.05)\n"
                "print('second line', flush=True)\n")
            result = {}
            worker = threading.Thread(target=lambda: result.update(zip(
                ("code", "out", "err", "duration"),
                landing_gate.run_cmd([sys.executable, "-c", script], Path(d), 60,
                                     live_log=live))))
            worker.start()
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and not (
                    live.exists() and "first line" in live.read_text()):
                time.sleep(0.05)
            seen_while_running = live.read_text() if live.exists() else ""
            gate.touch()
            worker.join(60)
            self.assertIn("first line", seen_while_running)
            self.assertNotIn("second line", seen_while_running)
            self.assertEqual(result["code"], 0)
            self.assertEqual(result["out"], "first line\nsecond line\n")

    def test_passing_leg_leaves_no_log_and_failing_leg_keeps_full_transcript(self):
        with tempfile.TemporaryDirectory() as d, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            landing_gate.run_check("ok-leg", [sys.executable, "-c", "print('fine')"], Path(d), 60)
            landing_gate.run_check("bad-leg", [sys.executable, "-c",
                "import sys; print('out'); print('why', file=sys.stderr); sys.exit(3)"],
                Path(d), 60)
            logs = {p.name.rsplit("-", 2)[0]: p.read_text()
                    for p in (Path(d) / "target/landing-logs").glob("*.log")}
        self.assertEqual(logs, {"bad-leg": "out\nwhy\n"})
        self.assertIn("[RUN] ok-leg  (live transcript:", output.getvalue())

    def test_nightly_keeps_full_renderer_coverage(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["trunk_health.py", "--dry-run"]))
            stack.enter_context(patch.object(trunk_health, "LOG_DIR", Path(d)))
            stack.enter_context(patch.object(trunk_health, "BD", "bd"))
            stack.enter_context(patch.object(trunk_health, "run_cmd", return_value=(0, "tip", "", 0)))
            stack.enter_context(patch.object(trunk_health.subprocess, "run",
                                            return_value=subprocess.CompletedProcess([], 0, "", "")))
            self.assertEqual(trunk_health.main(), 0)
        self.assertIn("would run: python3 scripts/gpu_proofs_gate.py --all", output.getvalue())


class DeliveryTests(unittest.TestCase):
    def test_only_explicit_named_red_collects_all_checks(self):
        cases = [([], False), (["--named-red", "BUG-test", "--reason", "reviewed"], True),
                 (["--named-red", "BUG-test"], False),
                 (["--named-red", "BUG-test", "--reason", "reviewed", "--skip-gpu", "deferred"], False)]
        for extra, expected in cases:
            with self.subTest(extra=extra), tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                stack.enter_context(patch.object(sys, "argv", ["land_branch.py", "codex/test", "--worktree", d, "--message", "test", *extra]))
                stack.enter_context(patch.object(land_branch, "step"))
                gate = stack.enter_context(patch.object(land_branch, "run_landing_gate", side_effect=RuntimeError("stop before delivery")))
                with self.assertRaisesRegex(RuntimeError, "stop before delivery"):
                    land_branch.main()
                self.assertEqual("--keep-going" in gate.call_args.args[0], expected)
                argv = gate.call_args.args[0]
                self.assertEqual(argv[argv.index("--repo") + 1], str(Path(d).resolve()))

    def test_progress_is_forwarded_before_child_exits(self):
        # A real child cannot finish until the parent's output sink observes
        # its first line. Buffered-until-completion implementations fail here.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            log = root / "gate.log"
            release = root / "release"

            class Output(io.StringIO):
                def write(self, text):
                    if text.startswith("ready"):
                        self_test.assertIn("ready", log.read_text())
                        release.touch()
                    return super().write(text)

            self_test = self
            script = (
                "import pathlib, sys, time\n"
                "print('ready', flush=True)\n"
                "deadline = time.monotonic() + 3\n"
                "while not pathlib.Path(sys.argv[1]).exists():\n"
                " if time.monotonic() > deadline: sys.exit(99)\n"
                " time.sleep(0.01)\n"
                "print('failed detail', file=sys.stderr, flush=True)\n"
                "sys.exit(7)\n"
            )
            with contextlib.redirect_stdout(Output()) as output:
                code = land_branch.run_landing_gate([sys.executable, "-u", "-c", script, str(release)], root, log)
            self.assertEqual(code, 7)
            self.assertIn("failed detail", output.getvalue())
            self.assertEqual(log.read_text(), "ready\nfailed detail\n")

    def test_failed_gate_never_merges_or_pushes(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["land_branch.py", "codex/test", "--worktree", d, "--message", "test"]))
            step = stack.enter_context(patch.object(land_branch, "step"))
            gate = stack.enter_context(patch.object(land_branch, "run_landing_gate", return_value=1))
            with self.assertRaises(SystemExit) as stopped:
                land_branch.main()
            self.assertEqual(stopped.exception.code, 1)
            self.assertEqual([call.args[0] for call in step.call_args_list], ["fetch", "merge origin/main into branch"])
            self.assertNotIn("--keep-going", gate.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
