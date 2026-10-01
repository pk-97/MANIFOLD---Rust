#!/usr/bin/env python3
"""Exercise gate sequencing and delivery without Cargo, rendering, or git writes."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import landing_gate
import land_branch
import landing_marker
import trunk_health


class LandingTests(unittest.TestCase):
    checks = ["tooling", "design-status", "docs-index", "deny", "ignored-tests",
              "clippy", "flow-gate", "tests", "gpu-proofs"]

    NEXTEST_RED = ("        FAIL [   0.1s] manifold-gpu core::hang\n"
                   "        FAIL [   0.1s] manifold-gpu core::other\n")
    GPU_RED = "test proofs::flip_glitch ... FAILED\ntest proofs::fine ... ok\n"

    def exercise(self, failed=None, extra=(), stale_docs=False, packages=True, head="head", paths=None,
                 red_output=None, still_red_on_main=(), main_at_origin=True, beads=None, dirty=""):
        """red_output maps a failed label to the failing run's stdout; still_red_on_main
        names tests that also fail when rerun in the main checkout; beads maps test
        name -> open bead id (a name not in it has no bead and bd filing fails)."""
        called, commands = [], []
        self.events = events = []
        self.reruns = reruns = []
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

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            root = Path(d).resolve()
            repo = root / "worktree"
            repo.mkdir()

            def run(cmd, cwd, timeout):
                commands.append(cmd)
                in_main = Path(cwd).resolve() == root
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
                    elif cmd[1] == "status":
                        out = dirty
                    elif cmd[1:] == ["rev-parse", "HEAD^{tree}"]:
                        out = "tree-of-head"
                    elif cmd[1:] == ["rev-parse", "HEAD"]:
                        out = ("origin-tip" if main_at_origin else "stale-tip") if in_main else head
                    elif cmd[1:] == ["rev-parse", "origin/main"]:
                        out = "origin-tip"
                    elif cmd[1] == "rev-parse":
                        out = "codex/test"
                    else:
                        out = ""
                    return 0, out, "", 0.01
                if in_main:
                    reruns.append(cmd)
                    names = {t for t in still_red_on_main}
                    lines = []
                    for name in names:
                        lines.append(f"test {name} ... FAILED" if cmd[1] == "test"
                                     else f"        FAIL [   0.1s] manifold-gpu {name}")
                    return 1 if lines else 0, "\n".join(lines) + "\n", "", 0.01
                label = ({"nextest": "tests"}.get(cmd[1], cmd[1]) if cmd[0] == "cargo"
                         else labels[Path(cmd[1]).name])
                called.append(label)
                events.append(label)
                out = (red_output or {}).get(label, f"output for {label}\n") if label == failed \
                    else f"output for {label}\n"
                return (1 if label == failed else 0), out, "", 0.01

            def find_bead(test):
                return (beads or {}).get(test)

            def file_bead(title, desc):
                return None, "bd is not available in the test"

            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["landing_gate.py", "--repo", str(repo), *extra]))
            stack.enter_context(patch.object(landing_gate, "MAIN_CHECKOUT", root))
            stack.enter_context(patch.object(landing_gate, "run_cmd", side_effect=run))
            stack.enter_context(patch.object(landing_gate.trunk_health, "find_open_bead", side_effect=find_bead))
            stack.enter_context(patch.object(landing_gate.trunk_health, "file_bead", side_effect=file_bead))
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
            marker_file = root / ".claude/orchestration/landing-gate-marker.json"
            self.marker = json.loads(marker_file.read_text()) if marker_file.exists() else None
            logs = [p.read_text() for p in (repo / "target/landing-logs").glob("*.log")]
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

    def test_green_run_writes_a_passing_marker_for_heads_tree(self):
        code, *_ = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(self.marker["tree"], "tree-of-head")
        self.assertIs(self.marker["pass"], True)
        self.assertEqual(self.marker["failing_tests"], [])
        self.assertEqual(self.marker["pre_existing_tests"], [])
        self.assertEqual(self.marker["schema"], landing_marker.SCHEMA)

    def test_red_run_writes_a_red_marker(self):
        code, *_ = self.exercise("clippy")
        self.assertEqual(code, 1)
        self.assertIs(self.marker["pass"], False)

    def test_dirty_tracked_files_fail_the_gate(self):
        # The marker names HEAD's tree; edits on top of it mean a different tree was checked.
        code, _, _, _, _, output, _ = self.exercise(dirty=" M crates/x.rs")
        self.assertEqual(code, 1)
        self.assertIn("clean-tree", output)
        self.assertIs(self.marker["pass"], False)

    def test_failure_that_also_fails_on_main_with_a_bead_is_pre_existing(self):
        code, called, _, _, _, output, _ = self.exercise(
            "tests", red_output={"tests": self.NEXTEST_RED},
            still_red_on_main=["core::hang", "core::other"],
            beads={"core::hang": "BUG-aaa", "core::other": "BUG-bbb"})
        self.assertEqual(code, 0)
        self.assertEqual(called, self.checks)
        self.assertIs(self.marker["pass"], True)
        self.assertEqual(self.marker["failing_tests"], [])
        self.assertEqual(sorted(e["bead"] for e in self.marker["pre_existing_tests"]),
                         ["BUG-aaa", "BUG-bbb"])
        self.assertIn("PRE-EXISTING manifold-gpu core::hang", output)
        # Exactly the failing tests were rerun, in the main checkout.
        self.assertEqual(len(self.reruns), 1)
        self.assertIn("binary_id(=manifold-gpu) & test(=core::hang)", " ".join(self.reruns[0]))

    def test_failure_that_passes_on_main_stays_red(self):
        code, _, _, _, _, output, _ = self.exercise(
            "tests", red_output={"tests": self.NEXTEST_RED}, still_red_on_main=[],
            beads={"core::hang": "BUG-aaa"})
        self.assertEqual(code, 1)
        self.assertIs(self.marker["pass"], False)
        self.assertEqual(self.marker["failing_tests"],
                         ["manifold-gpu core::hang", "manifold-gpu core::other"])
        self.assertIn("NEW FAILURE manifold-gpu core::hang", output)

    def test_one_new_failure_among_pre_existing_ones_stays_red(self):
        code, *_ = self.exercise(
            "tests", red_output={"tests": self.NEXTEST_RED}, still_red_on_main=["core::hang"],
            beads={"core::hang": "BUG-aaa"})
        self.assertEqual(code, 1)
        self.assertEqual(self.marker["failing_tests"], ["manifold-gpu core::other"])
        self.assertEqual([e["test"] for e in self.marker["pre_existing_tests"]],
                         ["manifold-gpu core::hang"])

    def test_pre_existing_failure_without_a_bead_stays_red(self):
        # No open bead and filing fails (the mock's bd is unavailable).
        code, _, _, _, _, output, _ = self.exercise(
            "tests", red_output={"tests": self.NEXTEST_RED}, still_red_on_main=["core::hang", "core::other"])
        self.assertEqual(code, 1)
        self.assertIs(self.marker["pass"], False)
        self.assertEqual(self.marker["pre_existing_tests"], [])
        self.assertIn("no bead and none could be filed", output)

    def test_rerun_requires_main_at_origin(self):
        code, *_ = self.exercise(
            "tests", red_output={"tests": self.NEXTEST_RED}, still_red_on_main=["core::hang", "core::other"],
            beads={"core::hang": "BUG-aaa", "core::other": "BUG-bbb"}, main_at_origin=False)
        self.assertEqual(code, 1)
        self.assertEqual(self.reruns, [])
        self.assertIs(self.marker["pass"], False)

    def test_gpu_proof_failing_on_main_with_a_bead_is_pre_existing(self):
        code, called, _, _, _, _, _ = self.exercise(
            "gpu-proofs", red_output={"gpu-proofs": self.GPU_RED},
            still_red_on_main=["proofs::flip_glitch"], beads={"proofs::flip_glitch": "BUG-gpu"})
        self.assertEqual(code, 0)
        self.assertEqual(called, self.checks)
        self.assertEqual(self.marker["pre_existing_tests"],
                         [{"test": "gpu-proofs proofs::flip_glitch", "bead": "BUG-gpu"}])
        rerun = self.reruns[0]
        self.assertEqual(rerun[:2], ["cargo", "test"])
        self.assertIn("gpu-proofs", rerun)
        self.assertEqual(rerun[-1], "proofs::flip_glitch")
        self.assertIn("--exact", rerun)
        # The rerun happens while the gate still holds the GPU lock.
        self.assertEqual(self.events[-1], "hold-exit")

    def test_gpu_proof_that_passes_on_main_stays_red(self):
        code, *_ = self.exercise(
            "gpu-proofs", red_output={"gpu-proofs": self.GPU_RED}, still_red_on_main=[],
            beads={"proofs::flip_glitch": "BUG-gpu"})
        self.assertEqual(code, 1)
        self.assertEqual(self.marker["failing_tests"], ["gpu-proofs proofs::flip_glitch"])

    def test_build_error_alongside_failures_stays_red(self):
        out = self.NEXTEST_RED + "error: could not compile `x`\n"
        code, *_ = self.exercise(
            "tests", red_output={"tests": out}, still_red_on_main=["core::hang", "core::other"],
            beads={"core::hang": "BUG-aaa", "core::other": "BUG-bbb"})
        self.assertEqual(code, 1)

    def test_keep_going_collects_every_check_but_the_marker_stays_red(self):
        code, called, timings, *_ = self.exercise("docs-index", extra=["--keep-going"])
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks)
        self.assertEqual(timings["failed"], 1)
        self.assertIs(self.marker["pass"], False)

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

    def test_timeout_retains_partial_output(self):
        error = subprocess.TimeoutExpired(["test"], 1, output=b"before timeout\n", stderr=b"detail\n")
        with patch.object(landing_gate.subprocess, "run", side_effect=error):
            code, out, err, _ = landing_gate.run_cmd(["test"], Path.cwd(), 1)
        self.assertEqual(code, -1)
        self.assertEqual(out, "before timeout\n")
        self.assertIn("detail", err)
        self.assertIn("TIMEOUT", err)

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


class MarkerTests(unittest.TestCase):
    def check(self, record, tree="T"):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "m.json"
            if record is not None:
                landing_marker.write_marker(record, path)
            return landing_marker.marker_problem(tree, path)

    def record(self, **over):
        base = {"schema": landing_marker.SCHEMA, "tree": "T", "pass": True,
                "failing_tests": [], "pre_existing_tests": []}
        base.update(over)
        return base

    def test_green_marker_for_the_same_tree_clears(self):
        self.assertIsNone(self.check(self.record()))

    def test_missing_marker_denies(self):
        self.assertIn("no landing-gate marker", self.check(None))

    def test_tree_mismatch_denies(self):
        self.assertIn("marker is for tree", self.check(self.record(tree="OTHER")))

    def test_red_marker_denies(self):
        self.assertIn("RED", self.check(self.record(**{"pass": False})))

    def test_failing_tests_deny_even_if_pass_is_true(self):
        self.assertIn("failing tests", self.check(self.record(failing_tests=["a b"])))

    def test_pre_existing_failure_with_a_bead_clears(self):
        self.assertIsNone(self.check(self.record(pre_existing_tests=[{"test": "a b", "bead": "BUG-x"}])))

    def test_pre_existing_failure_without_a_bead_denies(self):
        self.assertIn("no bead", self.check(self.record(pre_existing_tests=[{"test": "a b"}])))

    def test_unknown_schema_denies(self):
        self.assertIn("schema", self.check(self.record(schema=99)))

    def test_write_marker_replaces_atomically_and_leaves_no_temp_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "m.json"
            landing_marker.write_marker(self.record(tree="A"), path)
            landing_marker.write_marker(self.record(tree="B"), path)
            self.assertEqual(json.loads(path.read_text())["tree"], "B")
            self.assertEqual([p.name for p in Path(d).iterdir()], ["m.json"])


class DeliveryTests(unittest.TestCase):
    def land(self, gate_code=0, tree="tree-1", problem=None, extra=()):
        """Run land_branch.main with git and the gate stubbed; returns (steps, exit code)."""
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["land_branch.py", "codex/test", "--worktree", d, "--message", "test", *extra]))
            step = stack.enter_context(patch.object(land_branch, "step"))
            gate = stack.enter_context(patch.object(land_branch, "run_landing_gate", return_value=gate_code))
            stack.enter_context(patch.object(land_branch.landing_marker, "tree_of", return_value=tree))
            stack.enter_context(patch.object(land_branch.landing_marker, "marker_problem", return_value=problem))
            stack.enter_context(patch.object(land_branch.subprocess, "run",
                                             return_value=subprocess.CompletedProcess([], 1, "", "")))
            code = 0
            try:
                land_branch.main()
            except SystemExit as stopped:
                code = stopped.code
            self.gate_argv = gate.call_args.args[0] if gate.call_args else None
            self.worktree = str(Path(d).resolve())
            return [call.args[0] for call in step.call_args_list], code

    def test_gate_never_collects_everything_and_runs_on_the_worktree(self):
        self.land()
        self.assertNotIn("--keep-going", self.gate_argv)
        self.assertEqual(self.gate_argv[self.gate_argv.index("--repo") + 1], self.worktree)

    def test_the_retired_named_red_override_is_rejected(self):
        steps, code = self.land(extra=["--named-red", "BUG-test", "--reason", "reviewed"])
        self.assertEqual(code, 2)  # argparse: unrecognized arguments
        self.assertEqual(steps, [])

    def test_green_gate_with_a_clearing_marker_merges(self):
        steps, code = self.land()
        self.assertEqual(code, 0)
        self.assertIn("merge --no-ff to main", steps)
        self.assertIn("push main", steps)

    def test_a_marker_that_does_not_clear_the_tree_never_merges(self):
        steps, code = self.land(problem="marker is for tree aaa, the branch tip's tree is bbb")
        self.assertEqual(code, 1)
        self.assertNotIn("merge --no-ff to main", steps)
        self.assertNotIn("push main", steps)

    def test_unresolvable_tree_never_merges(self):
        steps, code = self.land(tree=None)
        self.assertEqual(code, 1)
        self.assertNotIn("merge --no-ff to main", steps)

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
