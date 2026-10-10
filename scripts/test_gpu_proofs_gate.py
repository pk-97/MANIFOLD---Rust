#!/usr/bin/env python3
"""Focused tests for the gpu-proofs cargo command builder."""

import contextlib
import io
import json
import shlex
import subprocess
import sys
import tempfile
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
    def test_builds_selected_targets_once_per_package_without_running_tests(self):
        runs = [
            dict(package="catalog", targets=[], lib=True, full=False),
            dict(package="catalog", targets=["gpu_proofs"], lib=False, full=False),
            dict(package="catalog", targets=["main"], lib=False, full=False),
            dict(package="catalog", targets=["gpu_proofs"], lib=False, full=False),
            dict(package="scene", targets=["gpu_proofs"], lib=False, full=False),
        ]
        with patch.object(gate.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as cargo:
            self.assertEqual(gate.build_tests(Path("/tmp/Cargo.toml"), runs), 0)
        commands = [call.args[0] for call in cargo.call_args_list]
        self.assertEqual(len(commands), 2)
        self.assertIn("--lib", commands[0])
        self.assertNotIn("--lib", commands[1])
        targets = lambda cmd: {cmd[i + 1] for i, arg in enumerate(cmd[:-1]) if arg == "--test"}
        self.assertEqual(targets(commands[0]), {"gpu_proofs", "main"})
        self.assertEqual(targets(commands[1]), {"gpu_proofs"})
        self.assertTrue(all("--no-run" in command and "--" not in command for command in commands))

    def test_build_tests_resolves_relative_manifest_and_sets_workspace_cwd(self):
        runs = [dict(package="catalog", targets=["gpu_proofs"], lib=False, full=False)]
        manifest = Path("nested/worktree/Cargo.toml")
        with patch.object(gate.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as cargo:
            self.assertEqual(gate.build_tests(manifest, runs), 0)
        call = cargo.call_args
        expected_manifest = (Path.cwd() / manifest).resolve()
        self.assertEqual(call.kwargs["cwd"], expected_manifest.parent)
        command = call.args[0]
        self.assertEqual(command[command.index("--manifest-path") + 1], str(expected_manifest))

    def test_run_gate_resolves_relative_manifest_and_sets_workspace_cwd(self):
        process = FakeProcess()
        manifest = Path("nested/worktree/Cargo.toml")
        with patch.object(gate.subprocess, "Popen", return_value=process) as popen:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(gate.run_gate(manifest, [], [])[0], 0)
        call = popen.call_args
        expected_manifest = (Path.cwd() / manifest).resolve()
        self.assertEqual(call.kwargs["cwd"], expected_manifest.parent)
        command = call.args[0]
        self.assertEqual(command[command.index("--manifest-path") + 1], str(expected_manifest))

    def test_full_package_build_subsumes_selected_targets_and_failure_stops_builds(self):
        runs = [dict(package="catalog", targets=["gpu_proofs"], lib=False, full=False),
                dict(package="catalog", targets=None, lib=False, full=True),
                dict(package="scene", targets=[], lib=True, full=False)]
        with patch.object(gate.subprocess, "run", return_value=subprocess.CompletedProcess([], 7)) as cargo:
            self.assertEqual(gate.build_tests(Path("/tmp/Cargo.toml"), runs), 7)
        cargo.assert_called_once()
        self.assertNotIn("--test", cargo.call_args.args[0])
        self.assertNotIn("--lib", cargo.call_args.args[0])

    def test_committed_allowances_have_package_target_test_keys(self):
        path = Path(__file__).with_name("gpu_test_times.json")
        rows = json.loads(path.read_text())["tests"]
        invalid = [key for key in rows
                   if len(key.split("/")) != 3
                   or any(part in ("", "?") for part in key.split("/"))]
        self.assertEqual(invalid, [], "proof allowances must name package/target/test")

    def test_ui_paint_command_uses_own_lib_binary(self):
        cmd = gate.cargo_test_cmd(Path("/tmp/Cargo.toml"), targets=[], lib=True,
                                  package="manifold-ui-paint")
        self.assertEqual(cmd[cmd.index("-p") + 1], "manifold-ui-paint")
        self.assertIn("--lib", cmd)
        self.assertNotIn("--test", cmd)

    def setUp(self):
        directory = self.enterContext(tempfile.TemporaryDirectory())
        self.learned = Path(directory) / "learned.json"
        self.enterContext(patch.object(gate.gpu_scope, "learned_times_path", return_value=self.learned))

    class Workspace:
        packages = {name: {"features": ["gpu-proofs"]} for name in
                    ("manifold-nodes", "manifold-node-engine", "manifold-ui-paint", "manifold-nodes-scene")}
        packages["manifold-nodes"]["features"].append("fluid-perf-proofs")

        def __init__(self, repo=None):
            self.repo = Path(repo or Path.cwd()).resolve()
            self.roots = {
                "manifold-nodes": "crates/manifold-nodes",
                "manifold-node-engine": "crates/manifold-node-engine",
                "manifold-ui-paint": "crates/manifold-ui-paint",
                "manifold-nodes-scene": "crates/manifold-nodes-scene",
            }

        def ownership_errors(self, paths, base=None):
            return []  # Scope tests use a synthetic, complete inventory.

        def feature_packages(self, feature):
            self.assert_feature = feature
            return list(self.packages)

        def targets(self, package, kind=None):
            targets = {
                "manifold-nodes": [
                    {"name": "renderer", "kind": ["lib"],
                     "src_path": str(self.repo / "crates/manifold-nodes/src/lib.rs")},
                    {"name": "gpu_proofs", "kind": ["test"],
                     "required-features": ["gpu-proofs"],
                     "src_path": str(self.repo / "crates/manifold-nodes/tests/gpu_proofs.rs")},
                    {"name": "glb_conformance", "kind": ["test"],
                     "src_path": str(self.repo / "crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs")},
                ],
                "manifold-node-engine": [{"name": "engine", "kind": ["lib"],
                                           "src_path": str(self.repo / "crates/manifold-node-engine/src/lib.rs")}],
                "manifold-ui-paint": [{"name": "paint", "kind": ["lib"],
                                        "src_path": str(self.repo / "crates/manifold-ui-paint/src/lib.rs")}],
                "manifold-nodes-scene": [{"name": "gpu_proofs", "kind": ["test"],
                                          "required-features": ["gpu-proofs"],
                                          "src_path": str(self.repo / "crates/manifold-nodes-scene/tests/gpu_proofs/main.rs")}],
            }[package]
            return [target for target in targets if kind is None or kind in target["kind"]]

        def binary_owner(self, target):
            return "manifold-nodes" if target in {"gpu_proofs", "glb_conformance"} else "manifold-nodes"

        def owner(self, path):
            if path.startswith("crates/manifold-node-engine/"):
                return "manifold-node-engine"
            if path.startswith("crates/manifold-ui-paint/"):
                return "manifold-ui-paint"
            if path.startswith("crates/manifold-nodes/"):
                return "manifold-nodes"
            return None

    def test_default_targets_only_gpu_proofs(self):
        command, _ = run_gate()

        self.assertEqual(command[command.index("--test") + 1], "gpu_proofs")
        self.assertIn("--no-fail-fast", command)
        self.assertEqual(command[command.index("--") + 1 :], ["--test-threads=1"])

    def test_extra_features_match_for_build_and_run(self):
        code, calls, _ = self.run_main([
            "--package", "manifold-nodes", "--test", "gpu_proofs",
            "--features", "fluid-perf-proofs",
        ])
        self.assertEqual(code, 0)
        expected = "gpu-proofs fluid-perf-proofs"
        commands = [command for kind, command in self.events if kind in {"build", "run"}]
        self.assertTrue(commands)
        self.assertEqual({command[command.index("--features") + 1] for command in commands}, {expected})
        self.assertEqual(calls[0]["features"], ["gpu-proofs", "fluid-perf-proofs"])

    def test_unsupported_extra_feature_is_rejected_before_build(self):
        code, calls, output = self.run_main([
            "--package", "manifold-nodes-scene", "--test", "gpu_proofs",
            "--features", "fluid-perf-proofs",
        ])
        self.assertEqual(code, 2)
        self.assertEqual(calls, [])
        self.assertIn("does not support proof feature", output)

    def test_explicit_package_uses_metadata_owned_library(self):
        code, calls, _ = self.run_main(["--package", "manifold-ui-paint", "--filter", "paint"])
        self.assertEqual(code, 0)
        self.assertEqual(calls[0]["package"], "manifold-ui-paint")
        self.assertTrue(calls[0]["lib"])
        self.assertEqual(calls[0]["targets"], [])

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
        self.assertEqual([(n, round(s, 1)) for n, s, _, status in timings], [("a::one", 3.0), ("a::two", 10.5)])
        self.assertEqual([t[3] for t in timings], ["ok", "FAILED"])

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
        self.assertEqual([(n, round(s, 1)) for n, s, _, status in timings], [("a::slow", 200.0), ("a::fast", 1.0)])

    def test_timings_collected_during_run(self):
        process = FakeProcess()
        process.stdout = iter(["     Running a (b)\n", "test x ... ok\n"])
        timings = []
        with patch.object(gate.subprocess, "Popen", return_value=process):
            ticks = iter([1.0, 4.0] + [4.0] * 50)
            with patch.object(gate.time, "monotonic", side_effect=lambda: next(ticks)):
                with contextlib.redirect_stdout(io.StringIO()):
                    gate.run_gate(Path("/tmp/Cargo.toml"), [], [], None, False, False, timings)
        self.assertEqual(timings, [("x", 3.0, "a", "ok")])

    def summary(self, timings, budget, exit_code=0):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", exit_code, timings, budget)
        return code, out.getvalue()

    def test_over_budget_passes_with_separate_warning_and_names_slowest(self):
        timings = [("fast", 10.0, "b", True), ("slow_one", 200.0, "b", True), ("slow_two", 120.0, "b", True)]
        code, text = self.summary(timings, 300)
        self.assertEqual(code, 0)
        self.assertIn("GPU-PROOFS BUDGET: OVER (330s > 300s", text)
        self.assertLess(text.index("slow_one"), text.index("slow_two"))
        self.assertIn("GPU-PROOFS GATE: PASS", text)

    def test_under_budget_passes_and_unbudgeted_runs_are_exempt(self):
        timings = [("a", 100.0, "b", True), ("glb_sweep", 957.0, "glb", False)]
        code, text = self.summary(timings, 300)
        self.assertEqual(code, 0)
        self.assertIn("GPU-PROOFS GATE: PASS", text)

    def test_failed_rerun_roundtrips_duplicate_target_owners(self):
        class DuplicateTargetWorkspace:
            packages = {
                "manifold-nodes": {"features": ["gpu-proofs"]},
                "manifold-nodes-scene": {"features": ["gpu-proofs"]},
            }

            def feature_packages(self, feature):
                self.assert_feature = feature
                return list(self.packages)

            def targets(self, package, kind=None):
                rows = [{"name": "gpu_proofs", "kind": ["test"],
                         "src_path": f"crates/{package}/tests/gpu_proofs.rs"},
                        {"name": package, "kind": ["lib"],
                         "src_path": f"crates/{package}/src/lib.rs"}]
                return [row for row in rows if kind is None or kind in row["kind"]]

            def binary_owner(self, target):
                raise AssertionError(f"bare target owner lookup: {target}")

        name = "rt_dynamic_current_frame::rt_dynamic_history_reset_and_resume"
        output = (f"failures:\n    {name}\n\n"
                  "test result: FAILED. 0 passed; 1 failed;\n"
                  f"failures:\n    {name}\n\n"
                  "test result: FAILED. 0 passed; 1 failed;\n")
        timings = [
            gate.timing_entry("manifold-nodes", "gpu_proofs", name,
                              1.0, "FAILED", True),
            gate.timing_entry("manifold-nodes-scene", "gpu_proofs", name,
                              1.0, "FAILED", True),
        ]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary(output, 1, timings,
                                      manifest_path=Path("/tmp/work tree/Cargo.toml"))
        self.assertNotEqual(code, 0)
        commands = [line.removeprefix("rerun: ") for line in out.getvalue().splitlines()
                    if line.startswith("rerun: ")]
        self.assertEqual(len(commands), 2)
        expected = ["manifold-nodes", "manifold-nodes-scene"]
        for command, package in zip(commands, expected):
            argv = shlex.split(command)
            self.assertEqual(argv[argv.index("--manifest-path") + 1],
                             "/tmp/work tree/Cargo.toml")
            self.assertEqual(argv[argv.index("--package") + 1], package)
            self.assertEqual(argv[argv.index("--test") + 1], "gpu_proofs")
            self.assertEqual(argv[argv.index("--filter") + 1], name)
            normalized = gate.normalize_runs(DuplicateTargetWorkspace(), [{
                "package": argv[argv.index("--package") + 1],
                "targets": [argv[argv.index("--test") + 1]],
                "lib": False,
                "full": False,
            }])
            self.assertEqual(normalized[0]["package"], package)
            self.assertEqual(normalized[0]["target"], "gpu_proofs")

    def test_failed_library_rerun_selects_package_without_test_target(self):
        name = "lib_only::broken"
        timings = [gate.timing_entry("manifold-ui-paint", "lib", name,
                                     1.0, "FAILED", True)]
        commands, unresolved = gate.failure_rerun_commands(
            [name], timings, Path("/tmp/work tree/Cargo.toml"))
        self.assertEqual(unresolved, [])
        argv = shlex.split(commands[0])
        self.assertEqual(argv[argv.index("--package") + 1], "manifold-ui-paint")
        self.assertNotIn("--test", argv)
        normalized = gate.normalize_runs(self.Workspace(), [{
            "package": "manifold-ui-paint",
            "targets": [],
            "lib": True,
            "full": False,
        }])
        self.assertEqual(normalized[0]["target"], "lib")

    def test_unidentified_failure_reports_missing_rerun_identity(self):
        name = "missing::identity"
        output = f"failures:\n    {name}\n\n" \
                 "test result: FAILED. 0 passed; 1 failed;\n"
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary(output, 1, [],
                                      manifest_path=Path("/tmp/Cargo.toml"))
        self.assertNotEqual(code, 0)
        self.assertIn(
            f"rerun unavailable: {name} (failed test has no package/target identity)",
            out.getvalue(),
        )

    def test_no_budget_never_fails_on_time(self):
        code, _ = self.summary([("a", 9999.0, "b", True)], None)
        self.assertEqual(code, 0)

    def run_main(self, argv, repo_changed=None, build_exit=0, measured=(), run_exit=0,
                 run_output="", run_hung=(), passed=None, owned_proofs=True):
        calls = []
        self.events = events = []

        def fake_run_gate(manifest, filters, skips, targets, full, lib, timings, hung=None,
                          hang_floor=None, package=None, target=None, budgeted=True,
                          target_names=None, features=None):
            calls.append(dict(filters=filters, skips=skips, targets=targets, full=full,
                              lib=lib, package=package, target=target, budgeted=budgeted,
                              features=features))
            events.append(("run", gate.cargo_test_cmd(manifest, targets, full, lib, package, features)))
            timings.extend(measured)
            if owned_proofs:
                timings.extend(gate.timing_entry(package, target, name + "proof", 0, "ok", budgeted)
                               for name in filters
                               if gate.gpu_scope.GPU_FILTER_TARGETS.get(name) == (package, target))
            hung.extend(run_hung)
            return run_exit, run_output

        def fake_build(cmd, **kwargs):
            if cmd[0] == "git":
                return subprocess.CompletedProcess(cmd, 0, stdout="testsha\n")
            self.assertEqual(kwargs["env"]["CARGO_INCREMENTAL"], "0")
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
            stack.enter_context(patch.object(gate.gate_passes, "proof_pass", return_value=passed))
            stack.enter_context(patch.object(gate, "Workspace", side_effect=self.Workspace))
            stack.enter_context(patch.object(gate.gpu_scope, "Workspace", side_effect=self.Workspace))
            stack.enter_context(patch.object(sys, "argv", ["gpu_proofs_gate.py", *argv]))
            stack.enter_context(patch.object(gate, "run_gate", side_effect=fake_run_gate))
            stack.enter_context(patch.object(gate.subprocess, "run", side_effect=fake_build))
            stack.enter_context(patch.object(gate, "changed_paths", return_value=repo_changed or []))
            stack.enter_context(patch.object(gate.gpu_queue, "hold", side_effect=recording_hold))
            stack.enter_context(contextlib.redirect_stdout(out))
            code = gate.main()
        return code, calls, out.getvalue()

    def test_child_failure_cannot_return_input_refusal_status(self):
        for stage in ("build", "run"):
            with self.subTest(stage=stage):
                code, _, _ = self.run_main(
                    ["--filter", "failed"], **{f"{stage}_exit": gate.INPUTS_CHANGED})
                self.assertEqual(code, 1)

    def test_input_changes_return_reserved_refusal_status(self):
        for checks in ([True], [False, True], [False, False, True]):
            with self.subTest(checks=checks), patch.object(
                    gate.gate_passes, "changed_passes", side_effect=checks):
                code, _, text = self.run_main(["--filter", "failed"])
                self.assertEqual(code, gate.INPUTS_CHANGED)
                self.assertIn("GPU-PROOFS GATE: FAIL (inputs changed", text)

    def test_test_binaries_build_before_the_hold_with_the_run_arguments(self):
        code, _, _ = self.run_main(
            [], repo_changed=["crates/manifold-nodes/src/registry.rs"])
        self.assertEqual(code, 0)
        kinds = [kind for kind, _ in self.events]
        packages = sorted({cmd[cmd.index("-p") + 1]
                           for kind, cmd in self.events if kind == "build"})
        self.assertEqual(
            kinds,
            ["build"] * 4 + ["hold-enter"] + ["run"] * 5 + ["hold-exit"],
        )
        builds = [cmd for kind, cmd in self.events if kind == "build"]
        runs = [cmd for kind, cmd in self.events if kind == "run"]
        self.assertCountEqual([cmd[cmd.index("-p") + 1] for cmd in builds], packages)
        self.assertEqual({cmd[cmd.index("-p") + 1] for cmd in runs}, set(packages))
        builds_by_package = {cmd[cmd.index("-p") + 1]: cmd for cmd in builds}
        for run in runs:
            package = run[run.index("-p") + 1]
            build = builds_by_package[package]
            if '--lib' in run:
                self.assertIn('--lib', build)
            for i, arg in enumerate(run[:-1]):
                if arg == '--test':
                    self.assertIn(run[i + 1], build)

    def test_reused_pass_with_saved_timing_red_reruns_only_that_test(self):
        saved = []
        passed = object.__new__(gate.gate_passes.Pass)
        passed.key, passed.label = 'k', 'gpu-proofs'
        passed.record = {'pass': True, 'seconds': 66.0, 'commit': 'c', 'time': 't',
                         'timings': [gate.timing_entry('manifold-nodes', 'gpu_proofs',
                                                       'cold_heavy', 66.0, 'ok', True)]}
        passed.reused = lambda: True
        passed.inputs_changed = False
        passed.unchanged = lambda: True
        passed.save = lambda code, secs=0, **kw: saved.append(kw['timings'])
        argv = ['--package', 'manifold-nodes', '--test', 'gpu_proofs', '--filter', 'cold_heavy']
        with patch.object(gate.gpu_scope, 'read_times', return_value={}):
            code, calls, text = self.run_main(
                argv, passed=passed, measured=[('cold_heavy', 1.7, 'gpu_proofs', 'ok')])
        self.assertEqual(code, 0)
        self.assertEqual([c['filters'] for c in calls], [['cold_heavy']])
        self.assertIn('rerunning only the flagged tests', text)
        self.assertEqual(gate.timing_fields(saved[0][0])[3], 1.7)

    def test_missing_owned_proof_fails_without_a_green_receipt(self):
        passed = object.__new__(gate.gate_passes.Pass)
        passed.key, passed.label, passed.record = 'earlier', 'gpu-proofs', None
        passed.unchanged = lambda: True
        passed.path = self.learned.parent / 'pass.json'
        passed.path.write_text('{"pass": true}')
        code, _, output = self.run_main(
            ['--package', 'manifold-nodes', '--test', 'gpu_proofs',
             '--filter', 'alpha_contract::effects_preserve_transparency'],
            passed=passed, owned_proofs=False)
        self.assertNotEqual(code, 0)
        self.assertIn('did not pass owned filters: alpha_contract::effects_preserve_transparency', output)
        self.assertFalse(passed.path.exists())

    def test_every_run_target_is_built_before_any_test(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs"])
        self.assertEqual(code, 0)
        self.assertGreater(len(calls), 1)
        kinds = [kind for kind, _ in self.events]
        first_hold = kinds.index("hold-enter")
        builds = [cmd for kind, cmd in self.events if kind == "build"]
        self.assertTrue(all(k == "build" for k in kinds[:first_hold]))
        self.assertNotIn("build", kinds[first_hold:])
        self.assertEqual(len(builds), len({tuple(b) for b in builds}))
        def artifacts(commands):
            selected = set()
            for cmd in commands:
                package = cmd[cmd.index('-p') + 1]
                if '--lib' in cmd:
                    selected.add((package, 'lib'))
                selected.update((package, cmd[i + 1])
                                for i, arg in enumerate(cmd[:-1]) if arg == '--test')
            return selected
        runs = [cmd for kind, cmd in self.events if kind == 'run']
        self.assertEqual(artifacts(builds), artifacts(runs))

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
        self.assertEqual([kind for kind, _ in self.events], ["build"] * len(self.Workspace.packages))
        self.assertIn("GPU-PROOFS GATE: BUILT", text)

    def test_default_is_scoped_from_diff_and_prints_mode(self):
        p = "crates/manifold-nodes/src/registry.rs"
        code, calls, text = self.run_main([], repo_changed=[p])
        self.assertEqual(code, 0)
        self.assertEqual({call['package'] for call in calls}, set(self.Workspace.packages))
        self.assertTrue(calls[0]["lib"])
        self.assertFalse(calls[0]["full"])
        self.assertIn("registry::", calls[0]["filters"])
        self.assertIn("GPU-PROOFS MODE: scoped", text)

    def test_default_with_no_gpu_paths_runs_nothing(self):
        code, calls, text = self.run_main([], repo_changed=["docs/X.md"])
        self.assertEqual((code, calls), (0, []))
        self.assertIn("nothing to run", text)

    def test_unmapped_path_fails_without_running(self):
        code, calls, text = self.run_main(
            [], repo_changed=["crates/manifold-nodes/tests/contracts/node_graph/x.bin"])
        self.assertEqual(code, 2)
        self.assertEqual(calls, [])
        self.assertIn("node_graph/x.bin", text)
        self.assertIn("no run-everything fallback", text)

    def test_all_flag_runs_full_suite_and_prints_mode(self):
        code, calls, text = self.run_main(["--all"])
        self.assertEqual({call["package"] for call in calls},
                         set(self.Workspace.packages))
        self.assertTrue(all(call["full"] for call in calls))
        self.assertIn("GPU-PROOFS MODE: all", text)

    def test_ui_paint_build_and_run_target_its_own_lib(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-ui-paint/src/ui_renderer.rs"])
        self.assertEqual(code, 0)
        paint = next(call for call in calls if call['package'] == 'manifold-ui-paint')
        self.assertTrue(paint["lib"])
        self.assertEqual(paint["targets"], [])
        builds = [cmd for kind, cmd in self.events if kind == "build" and 'manifold-ui-paint' in cmd]
        runs = [cmd for kind, cmd in self.events if kind == "run" and 'manifold-ui-paint' in cmd]
        self.assertEqual(builds, [cmd + ["--no-run"] for cmd in runs])

    def test_all_keeps_slow_tests_and_scoped_preserves_owning_slow_tests(self):
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
                [], repo_changed=["crates/manifold-nodes-water/src/primitives/matter_fill.rs"])
            self.assertNotIn("m::slow", calls[0]["skips"])
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
            text = gate.write_times_json(out, [("m::was_slow", 10.0, "b", True, "ok"),
                                               ("m::new_slow", 70.0, "b", True, "ok")])
        data = json.loads(out.read_text())
        self.assertEqual(data["tests"], {"m::new_slow": 70.0, "m::was_slow": 10.0})
        self.assertIn("now fast: m::was_slow", text)
        self.assertIn("new SLOW: m::new_slow", text)
        self.assertEqual(committed.read_text(), before)

    def test_over_budget_message_says_how_to_fix(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", 0, [("slow", 400.0, "b", True)], 300)
        self.assertEqual(code, 0)
        self.assertIn("retain timings automatically", out.getvalue())
        self.assertIn("Shorten", out.getvalue())

    def test_first_slow_pass_is_learned_and_next_scoped_run_skips_it(self):
        name = "liquid_conformance::new_slow_test"
        path = gate.gpu_scope.PROOFS_DIR + "liquid_conformance.rs"
        code, calls, text = self.run_main(["--learn-times", "--budget", "360"], [path],
                                         measured=[(name, 401, "b", "ok")])
        self.assertEqual(code, 5)
        self.assertNotIn(name, calls[0]["skips"])
        self.assertIn("GPU-PROOFS TIMING: FAIL", text)
        self.assertFalse(self.learned.exists())
        code, calls, _ = self.run_main([], [path])
        self.assertNotIn(name, calls[0]["skips"])
        code, calls, _ = self.run_main(["--all"])
        self.assertEqual(calls[0]["skips"], [])

    def test_nightly_pass_is_learned_without_record_times_flag(self):
        self.run_main(["--all", "--learn-times"], measured=[("new_nightly_slow", 80, "b", "ok")])
        self.assertEqual(gate.gpu_scope.load_times()["manifold-nodes/b/new_nightly_slow"], 80)

    def test_failure_over_budget_stays_red_and_is_not_learned(self):
        for exit_code, output in [(101, ""), (0, "test result: FAILED. 0 passed; 1 failed;\n")]:
            code, _, text = self.run_main(["--filter", "failed", "--learn-times", "--budget", "1"],
                                          measured=[("failed_slow", 401, "b", "FAILED")],
                                          run_exit=exit_code, run_output=output)
            self.assertNotEqual(code, 0)
            self.assertIn("GPU-PROOFS GATE: FAIL", text)
            self.assertFalse(self.learned.exists())

    def test_hung_measurements_are_not_learned(self):
        gate.remember_times([("hung", 401, "b", True, "ok")], 0, [("hung", 401)])
        self.assertFalse(self.learned.exists())

    def test_hang_through_main_invalidates_prior_pass_and_learns_nothing(self):
        # Use the real save failure path with an earlier run's on-disk pass.
        passed = object.__new__(gate.gate_passes.Pass)
        passed.key = "earlier"
        passed.label = 'gpu-proofs'
        passed.unchanged = lambda: True  # This case tests failed-run invalidation.
        passed.record = None  # The earlier pass was not reusable at lookup.
        passed.path = self.learned.parent / "pass.json"
        passed.path.write_text('{"pass": true}')
        code, _, text = self.run_main(
            ["--filter", "m::", "--learn-times"], passed=passed,
            measured=[("m::completed", 10, "b", "ok")],
            run_hung=[("m::stuck", 301)], run_exit=1)
        self.assertEqual(code, 4)
        self.assertIn("HUNG m::stuck", text)
        self.assertFalse(passed.path.exists())
        self.assertFalse(self.learned.exists())

    def test_record_times_on_red_run_excludes_failed_test_and_can_get_faster(self):
        dest = self.learned.parent / "export.json"
        dest.write_text(json.dumps({"tests": {"m::fast": 100, "m::red": 200}}))
        code, _, text = self.run_main(
            ["--filter", "m::", "--learn-times", "--record-times", str(dest)],
            measured=[("m::fast", 1, "b", "ok"), ("m::red", 126.2, "b", "FAILED")],
            run_exit=101)
        self.assertEqual(code, 101)
        times = json.loads(dest.read_text())["tests"]
        self.assertEqual(times, {"m::fast": 100, "m::red": 200})
        self.assertFalse(self.learned.exists())
        self.assertIn("cargo exit 101, no test failure parsed — not the budget", text)

    def test_ad_hoc_filter_does_not_learn(self):
        self.run_main(["--filter", "m::"], measured=[("m::slow", 100, "b", "ok")])
        self.assertFalse(self.learned.exists())

    def test_forget_removes_only_named_shared_entry_without_running(self):
        self.learned.write_text(json.dumps({"tests": {
            "a": {"s": 90, "sha": "old", "at": 1},
            "b": {"s": 80, "sha": "old", "at": 1}}}))
        code, calls, _ = self.run_main(["--forget", "a"])
        self.assertEqual((code, calls), (0, []))
        self.assertEqual(set(json.loads(self.learned.read_text())["tests"]), {"b"})

    def test_latest_run_replaces_cost_and_preserves_other_entries(self):
        gate.remember_times([("a", 90, "b", True, "ok")], 0, [])
        gate.remember_times([("b", 80, "b", True, "ok"), ("a", 1, "b", True, "ok")], 0, [])
        times = json.loads(self.learned.read_text())["tests"]
        self.assertEqual((times["a"]["s"], times["b"]["s"]), (1, 80))
        self.assertEqual(set(times["a"]), {"s", "sha", "at"})

    def test_record_merge_preserves_unmeasured_seed_and_destination_entries(self):
        with patch.object(gate.gpu_scope, "read_times", side_effect=[{"seed": 90}, {"old": 80}]):
            gate.write_times_json(self.learned, [("new", 70, "b", True, "ok")], merge=True)
        self.assertEqual(json.loads(self.learned.read_text())["tests"],
                         {"seed": 90, "old": 80, "new": 70})

    def test_explicit_filter_bypasses_scoping(self):
        code, calls, text = self.run_main(["--filter", "water_"], repo_changed=["docs/X.md"])
        self.assertEqual(calls[0]["filters"], ["water_"])
        self.assertIn("GPU-PROOFS MODE: explicit", text)

    def test_gltf_paths_add_a_separate_unbudgeted_glb_run(self):
        code, calls, _ = self.run_main(
            [], repo_changed=["crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs"])
        sweeps = [c for c in calls if not c['budgeted']]
        self.assertEqual(len(sweeps), 1)
        self.assertEqual(sweeps[0]["targets"], ["glb_conformance"])
        self.assertEqual(sweeps[0]["filters"], [])


class WatchdogTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(gate.gpu_scope, "learned_times_path", return_value=None))

    TIMES = {"m::known": 100.0}

    def dog(self, floor=None):
        return gate.Watchdog(self.TIMES, floor)

    def test_real_run_does_not_fall_back_to_ambiguous_name_only_timing(self):
        d = gate.Watchdog({"other/target/shared": 999.0}, package="pkg", target="target")
        self.assertEqual(d.allowance("shared"), gate.NO_RECORD_ALLOWANCE_S)
        keyed = gate.Watchdog({"pkg/target/shared": 100.0}, package="pkg", target="target")
        self.assertEqual(keyed.allowance("shared"), 500.0)

    def test_allowance_is_floor_five_times_record_or_no_record_default(self):
        d = self.dog()
        self.assertEqual(d.allowance("m::known"), 500.0)
        self.assertEqual(d.allowance("m::unknown"), 300.0)
        d.times = {"m::tiny": 2.0}
        self.assertEqual(d.allowance("m::tiny"), 120.0)

    def test_committed_glb_sweep_allowance_outlasts_a_whole_sweep(self):
        # The sweep takes ~16 minutes; the no-record 300s killed every glTF landing.
        d = gate.Watchdog(gate.gpu_scope.load_times(),
                          package="manifold-nodes", target="gpu_proofs")
        self.assertGreater(d.allowance("glb_conformance::glb_conformance_sweep"), 2 * 930.0)

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

    def test_new_heavy_proof_is_red_until_explicitly_measured(self):
        finding = {"package": "pkg", "target": "proofs", "test": "new_heavy",
                   "seconds": 61.0, "key": "pkg/proofs/new_heavy"}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.print_summary("", 0, [], None, [], Path("/tmp/Cargo.toml"), [finding])
        self.assertEqual(code, 5)
        text = out.getvalue()
        self.assertIn("GPU-PROOFS TIMING: FAIL", text)
        self.assertIn("--package pkg", text)
        self.assertIn("--test proofs", text)
        self.assertIn("--hang-allowance 300", text)


class TimingRerunTests(unittest.TestCase):
    RUN = dict(package="pkg", target="proofs", targets=["proofs"], lib=False, full=False,
               budgeted=True, features=None)
    FINDING = {"package": "pkg", "target": "proofs", "test": "cold_heavy",
               "seconds": 66.0, "key": "pkg/proofs/cold_heavy"}

    def rerun(self, seconds, passes=None, status="ok", code=0):
        saved = []
        records = {0: [gate.timing_entry("pkg", "proofs", "cold_heavy", 66.0, "ok", True),
                       gate.timing_entry("pkg", "proofs", "other", 1.0, "ok", True)]}

        def fake_run_gate(manifest, filters, skips, targets, full, lib, timings, *rest, **kwargs):
            saved.append(filters)
            timings.append(gate.timing_entry("pkg", "proofs", "cold_heavy", seconds, status, True))
            return code, ""

        class Receipt:
            def save(self, code, secs, *, timings=None, failed=None):
                saved.append(("save", secs, timings))

        @contextlib.contextmanager
        def hold(label, **kwargs):
            yield

        with patch.object(gate, "run_gate", side_effect=fake_run_gate), \
                patch.object(gate, "build_tests", return_value=0), \
                patch.object(gate.gpu_queue, "hold", side_effect=hold), \
                contextlib.redirect_stdout(io.StringIO()):
            fresh = gate.rerun_timing_red([self.FINDING], [dict(self.RUN)],
                                          [Receipt()], records, Path("/tmp/Cargo.toml"))
        return fresh, saved, records

    def test_only_flagged_tests_rerun_and_fast_time_clears_the_red(self):
        fresh, saved, records = self.rerun(1.7)
        self.assertEqual(saved[0], ["cold_heavy"])
        self.assertEqual([gate.timing_fields(t)[3] for t in records[0]], [1.7, 1.0])
        self.assertEqual(saved[1][0:2], ("save", 2.7))
        merged = gate._swap_timings(list(records[0]), fresh)
        with patch.object(gate.gpu_scope, "read_times", return_value={}):
            self.assertEqual(gate.unmeasured_heavy(merged), [])

    def test_slow_rerun_stays_red(self):
        fresh, _, records = self.rerun(70.0)
        with patch.object(gate.gpu_scope, "read_times", return_value={}):
            self.assertEqual([f["test"] for f in gate.unmeasured_heavy(records[0])], ["cold_heavy"])
        self.assertEqual(gate.timing_fields(list(fresh.values())[0])[3], 70.0)

    def test_failed_rerun_changes_nothing(self):
        fresh, saved, records = self.rerun(1.0, status="FAILED", code=101)
        self.assertEqual(fresh, {})
        self.assertEqual(gate.timing_fields(records[0][0])[3], 66.0)



if __name__ == "__main__":
    unittest.main()
