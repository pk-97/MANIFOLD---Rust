#!/usr/bin/env python3
"""Exercise gate sequencing and delivery without Cargo, rendering, or git writes."""
import contextlib
import io
import json
import os
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
import cpu_scope
import diff_scope


def process_alive(pid):
    """True while `pid` runs; a reaped or zombie process counts as gone."""
    try:
        state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                               capture_output=True, text=True).stdout.strip()
    except PermissionError:
        # macOS sandbox can deny ps while allowing checks of our own children.
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
    return bool(state) and not state.startswith("Z")


class LandingTests(unittest.TestCase):
    checks = ["tooling", "design-status", "docs-index", "deny", "ignored-tests",
              "clippy", "flow-gate", "tests-build", "gpu-proofs-build", "tests", "gpu-proofs"]

    def exercise(self, failed=None, extra=(), stale_docs=False, packages=True, head="head", paths=None,
                 comment=False):
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
            if "test(regenerates_in_sync)" in cmd:
                label = "catalog-fresh"
            if "--no-run" in cmd or "--build-only" in cmd:
                label += "-build"
            called.append(label)
            events.append(label)
            return (1 if label == failed else 0), f"output for {label}\n", "", 0.01

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            root = Path(d)
            if packages:
                for path in paths:
                    if path.startswith("crates/"):
                        crate = root / "crates" / path.split("/")[1]
                        crate.mkdir(parents=True, exist_ok=True)
                        (crate / "Cargo.toml").write_text(f'[package]\nname = "{crate.name}"\n')
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["landing_gate.py", "--repo", d, *extra]))
            stack.enter_context(patch.object(landing_gate, "MAIN_CHECKOUT", root))
            stack.enter_context(patch.object(landing_gate, "run_cmd", side_effect=run))
            # A tooling self-test must never wait on the machine-wide GPU lock.
            stack.enter_context(patch.object(landing_gate.gpu_queue, "hold",
                                             side_effect=recording_hold))
            stack.enter_context(patch.object(diff_scope, "effective_paths",
                                            return_value=([], paths) if comment else (paths, [])))
            deps = stack.enter_context(patch.object(landing_gate, "reverse_deps", return_value=[]))
            stack.enter_context(patch("codex_checks.tooling_checks", return_value=[] if comment else [{
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
        self.assertIn(["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs",
                       "--budget", "360"], commands)
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
        # Cheap prerequisites, clippy and every test-binary compile run
        # before the hold, so it covers test time only.
        self.assertLess(events.index("clippy"), enter)
        self.assertLess(events.index("tests-build"), enter)
        self.assertLess(events.index("gpu-proofs-build"), enter)

    def test_builds_compile_exactly_what_the_held_legs_run(self):
        _, _, _, commands, *_ = self.exercise()
        nextest = [c for c in commands if c[:2] == ["cargo", "nextest"]]
        selection = ["-p", "manifold-gpu",
                     "-E", "(package(=manifold-gpu) & test(/^metal::device::/))"]
        self.assertEqual(nextest, [["cargo", "nextest", "run", "--no-run", *selection],
                                   ["cargo", "nextest", "run", "--no-fail-fast", "--no-tests=pass", *selection]])
        proofs = [c for c in commands if c[1:2] == ["scripts/gpu_proofs_gate.py"]]
        self.assertEqual(proofs, [
            ["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs", "--build-only"],
            ["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs", "--budget", "360"]])

    def test_catalog_check_skipped_when_renderer_untouched(self):
        _, _, _, commands, *_ = self.exercise()
        self.assertFalse(any("test(regenerates_in_sync)" in c for c in commands))

    def test_proofs_do_not_change_nextest_selection(self):
        # Cover nested gpu_tests, scene modules, individually gated tests,
        # required-features binaries, and the transitive manifold-gpu feature.
        # Compare entire argv: no feature, package or filter changes may leak
        # from proof selection into nextest (including build/catalog commands).
        for path in (
            "crates/manifold-renderer/src/node_graph/primitives/invert.rs",
            "crates/manifold-renderer/src/node_graph/primitives/mod.rs",
            "crates/manifold-renderer/src/node_graph/primitives/gpu_flip_scene_tests.rs",
            "crates/manifold-renderer/src/node_graph/bundled_presets.rs",
            "crates/manifold-renderer/tests/gpu_proofs/main.rs",
            "crates/manifold-renderer/tests/glb_conformance.rs",
            "crates/manifold-gpu/src/metal/device.rs",
        ):
            with self.subTest(path=path):
                code, _, _, enabled, *_ = self.exercise(paths=[path])
                self.assertEqual(code, 0)
                code, _, _, disabled, *_ = self.exercise(
                    paths=[path], extra=["--skip-gpu", "deferred"])
                self.assertEqual(code, 0)
                nextest = lambda commands: [c for c in commands if c[:2] == ["cargo", "nextest"]]
                self.assertTrue(nextest(enabled))
                self.assertEqual(nextest(enabled), nextest(disabled))
                for cmd in nextest(enabled):
                    self.assertNotIn("--features", cmd)
                    self.assertNotIn("--all-features", cmd)
                self.assertIn(
                    ["python3", "scripts/gpu_proofs_gate.py", "--path", path,
                     "--budget", "360"], enabled)

    def test_skipped_proofs_do_not_enable_proof_features(self):
        code, _, _, commands, *_ = self.exercise(extra=["--skip-gpu", "deferred"])
        self.assertEqual(code, 0)
        self.assertTrue(all("--features" not in c for c in commands))

    def test_gate_builds_disable_incremental_and_preserve_jobs(self):
        from storage_budget import BuildCheck
        root = Path.cwd()
        with patch.dict(os.environ, {"CARGO_INCREMENTAL": "1", "CARGO_BUILD_JOBS": "4"}, clear=True), \
                patch("storage_budget.check_build", return_value=BuildCheck(True, root / "target", 200 * 2**30)):
            for cmd in (["cargo", "nextest"], ["python3", "scripts/gpu_proofs_gate.py"],
                        ["python3", "scripts/run_ui_flows.py"]):
                env, refusal = landing_gate.build_environment(cmd, root)
                self.assertIsNone(refusal)
                self.assertEqual(env["CARGO_INCREMENTAL"], "0")
                self.assertEqual(env["CARGO_BUILD_JOBS"], "4")

    def test_stale_docs_reported_but_stale_thumbnails_ignored(self):
        with tempfile.TemporaryDirectory() as d:
            assets = Path(d) / "crates/manifold-renderer/assets"
            for sub in ("effect-presets", "preset-thumbnails/effects"):
                (assets / sub).mkdir(parents=True)
            (assets / "effect-presets/Bloom.json").write_text("{}")
            (assets / "preset-thumbnails/effects/Bloom.hash").write_text("deadbeef")
            (Path(d) / "docs").mkdir()
            (Path(d) / "docs/README.md").write_text("old")
            (Path(d) / "docs/A.md").write_text("# A\n\nA long enough summary line for the index here.\n")
            names = [n for n, _, _ in landing_gate.freshness_problems(d)]
            self.assertEqual(names, ["docs-index"])

    def test_stale_artifacts_fail_gate_with_regenerate_commands(self):
        problems = [("docs-index", ["docs/README.md"], "regen-cmd")]
        with patch.object(landing_gate, "freshness_problems", return_value=problems):
            code, called, timings, _, _, output, _ = self.exercise()
        self.assertEqual(code, 1)
        self.assertEqual(called, [])
        self.assertIn("regenerate: regen-cmd", output)

    def test_build_failure_stops_before_the_hold(self):
        for failed in ("tests-build", "gpu-proofs-build"):
            with self.subTest(failed=failed):
                code, *_ = self.exercise(failed)
                self.assertEqual(code, 1)
                self.assertFalse(any(e.startswith("hold") for e in self.events))

    def test_tests_failure_still_releases_the_hold(self):
        self.exercise("tests")
        self.assertEqual(self.events[-1], "hold-exit")

    def test_unmapped_gpu_path_fails_gate_naming_path(self):
        code, called, _, _, _, output, _ = self.exercise(
            paths=["crates/manifold-renderer/src/node_graph/orphan.bin"])
        self.assertEqual(code, 1)
        self.assertNotIn("gpu-proofs", called)
        self.assertNotIn("gpu-proofs-build", called)
        self.assertNotIn("tests-build", called)
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
        self.assertIn("[SKIP] tests (no changed Rust modules or mapped integration binaries)", output)
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
        self.assertIn("would run: cargo nextest run --workspace", output.getvalue())

    def test_comment_only_rust_skips_builds_tests_and_gpu_without_hold(self):
        code, called, _, commands, _, output, deps = self.exercise(comment=True)
        self.assertEqual(code, 0)
        for label in ("clippy", "tests-build", "gpu-proofs-build", "tests", "gpu-proofs"):
            self.assertNotIn(label, called)
            self.assertIn(f"[SKIP] {label} (docs/comment-only diff)", output)
        self.assertFalse(any(e.startswith("hold") for e in self.events))
        self.assertFalse(any(c[:2] == ["cargo", "nextest"] for c in commands))
        self.assertEqual(deps, 0)

    def test_one_primitive_selects_only_its_module(self):
        path = "crates/manifold-renderer/src/node_graph/primitives/camera_lens.rs"
        _, _, _, commands, _, output, _ = self.exercise(paths=[path])
        expected = "(package(=manifold-renderer) & test(/^node_graph::primitives::camera_lens::/))"
        scoped = [c for c in commands if c[:2] == ["cargo", "nextest"] and "test(regenerates_in_sync)" not in c]
        self.assertEqual(len(scoped), 2)
        self.assertTrue(all(c[c.index("-E") + 1] == expected for c in scoped))
        self.assertIn("[tests] filterset: " + expected, output)


class DiffScopeTests(unittest.TestCase):
    def check_diff(self, before, after, suffix=".rs"):
        import difflib
        patch_text = "".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True), n=0))
        return diff_scope.comment_only(patch_text, before, after, suffix)

    def test_comments_and_block_interiors(self):
        for before, after, suffix in [
            ("// old\nfn f() {}\n", "/// new\nfn f() {}\n", ".rs"),
            ("//! old\n", "//! new\n\n", ".rs"),
            ("/*\nold\n*/\nfn f() {}\n", "/*\nnew\n*/\nfn f() {}\n", ".rs"),
            ("/* a /* nested */ b */\n", "/* x /* nested */ y */\n", ".rs"),
            ("// old\n", "// new\n", ".wgsl"),
            ("# old\nx = 1\n", "# new\nx = 1\n", ".py"),
        ]:
            with self.subTest(suffix=suffix, before=before):
                self.assertTrue(self.check_diff(before, after, suffix))

    def test_code_strings_and_comment_delimiters_stay_active(self):
        for before, after, suffix in [
            ("fn f() {} // old\n", "fn f() {} // new\n", ".rs"),
            ('let s = r#"\n// old\n"#;\n', 'let s = r#"\n// new\n"#;\n', ".rs"),
            ('let s = "\n\n";\n', 'let s = "\n\n\n";\n', ".rs"),
            ('s = """\n# old\n"""\n', 's = """\n# new\n"""\n', ".py"),
            ("/*\n*/\nfn f() {}\n", "/*\nfn f() {}\n*/\n", ".rs"),
        ]:
            with self.subTest(before=before):
                self.assertFalse(self.check_diff(before, after, suffix))

    def test_changed_lines_drive_path_exclusion(self):
        path = "crates/manifold-renderer/src/node_graph/primitives/blur.rs"
        def git(repo, *args):
            if "--name-only" in args:
                return path + "\0"
            if args[0] == "diff":
                return "@@ -1 +1 @@\n-// old\n+// new\n"
            return "// old\nfn f() {}\n" if args[1].startswith("base:") else "// new\nfn f() {}\n"
        with patch.object(diff_scope, "git", side_effect=git):
            self.assertEqual(diff_scope.effective_paths(Path.cwd(), "base"), ([], [path]))

    def test_sibling_alias_and_integration_mapping(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-renderer"
            src = crate / "src/node_graph"
            src.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-renderer"\n')
            (src / "fluid.rs").write_text('#[path = "fluid_tests.rs"]\nmod checks;\n')
            (src / "fluid_tests.rs").write_text("")
            plan = cpu_scope.plan_for_paths(["crates/manifold-renderer/src/node_graph/fluid.rs"], d)
            self.assertIn("test(/^node_graph::fluid::checks::/)", plan.filterset)
            self.assertIn("binary(=fluid_preset)", plan.filterset)

    def test_bundled_preset_json_selects_preset_contracts(self):
        with tempfile.TemporaryDirectory() as d:
            plan = cpu_scope.plan_for_paths(["crates/manifold-renderer/assets/generator-presets/Water.json"], d)
            self.assertEqual(plan.filterset, "(package(=manifold-renderer) & test(/^node_graph::bundled_presets::/))")
            self.assertEqual(plan.packages, {"manifold-renderer"})

    def test_flow_scope_uses_only_effective_paths(self):
        import run_ui_flows
        manifest = {"path_triggers": {"crates/manifold-ui/": ["ui"]}}
        with patch.object(diff_scope, "git", return_value="base\n"), \
                patch.object(diff_scope, "effective_paths", return_value=([], ["crates/manifold-ui/src/lib.rs"])) as scope:
            self.assertEqual(run_ui_flows.filters_for_touched("origin/main...HEAD", manifest), ([], {}))
            scope.assert_called_once_with(run_ui_flows.ROOT, "base", "HEAD")


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
            self.assertEqual([call.args[0] for call in step.call_args_list],
                             ["fetch", "merge origin/main into branch", "pin gate commit"])
            self.assertNotIn("--keep-going", gate.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
