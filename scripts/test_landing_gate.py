#!/usr/bin/env python3
"""Exercise gate sequencing and delivery without Cargo, rendering, or git writes."""
import contextlib
import io
import json
import os
import runpy
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

import landing_gate
import land_branch
import trunk_health
import cpu_scope
import diff_scope
import bridge_probe_gate
import gate_readiness
import gpu_scope
from gate_workspace import Workspace


def synthetic_workspace(root, paths):
    """Cargo metadata-shaped inventory for planning tests.

    Readiness deliberately treats Cargo metadata failure as a red. These tests
    exercise selection with a complete in-memory inventory instead of relying
    on incomplete temporary manifests or a host workspace.
    """
    root = Path(root).resolve()
    packages = sorted({path.split("/")[1] for path in paths if path.startswith("crates/")})
    if any(path.startswith(cpu_scope.CATALOG_PATHS) or path.endswith(".wgsl") for path in paths):
        packages = sorted(set(packages) | {"manifold-nodes"})
    if not packages:
        packages = ["manifold-app"]
    metadata = {"workspace_members": [], "packages": []}
    for package in packages:
        crate = root / "crates" / package
        crate.mkdir(parents=True, exist_ok=True)
        manifest = crate / "Cargo.toml"
        manifest.write_text(f'[package]\nname = "{package}"\nversion = "0.1.0"\nedition = "2021"\n')
        source = crate / "src/lib.rs"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.touch()
        for path in paths:
            if path.startswith(f"crates/{package}/") and Path(path).suffix:
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.touch(exist_ok=True)
        targets = [{"name": package, "kind": ["lib"], "src_path": str(source),
                    "required-features": []}]
        proof = crate / "tests/gpu_proofs/main.rs"
        proof.parent.mkdir(parents=True, exist_ok=True)
        proof.touch(exist_ok=True)
        targets.append({"name": "gpu_proofs", "kind": ["test"],
                        "src_path": str(proof), "required-features": ["gpu-proofs"]})
        glb = crate / "tests/glb_conformance.rs"
        glb.parent.mkdir(parents=True, exist_ok=True)
        glb.touch(exist_ok=True)
        if package == "manifold-nodes":
            targets.append({"name": "glb_conformance", "kind": ["test"],
                            "src_path": str(glb), "required-features": ["gpu-proofs"]})
            for binary in ("uniform_layout_proof", "uniform_layout_extended"):
                binary_source = crate / "src" / "bin" / f"{binary}.rs"
                binary_source.parent.mkdir(parents=True, exist_ok=True)
                binary_source.touch(exist_ok=True)
                targets.append({"name": binary, "kind": ["bin"],
                                "src_path": str(binary_source), "required-features": []})
        metadata["packages"].append({
            "id": f"path+file:///{package}", "name": package,
            "manifest_path": str(manifest), "dependencies": [],
            "features": {"gpu-proofs": []}, "targets": targets,
        })
        metadata["workspace_members"].append(f"path+file:///{package}")
    (root / "Cargo.toml").write_text(
        "[workspace]\nmembers = [\n" +
        "".join(f'    "crates/{package}",\n' for package in packages) + "]\n")
    return Workspace(root, metadata=metadata)


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


class SlowTestParsingTests(unittest.TestCase):
    def test_status_lines_colours_counters_and_summary_duplicates(self):
        transcript = (
            '    \x1b[33mSLOW\x1b[0m [>30.000s] manifold-nodes preset_runtime::tests::foo\n'
            '    SLOW [>60.000s] manifold-nodes preset_runtime::tests::foo\n'
            '    \x1b[32mPASS\x1b[0m [  62.345s] (1/3) \x1b[1mmanifold-nodes\x1b[0m preset_runtime::tests::foo\n'
            '    PASS [  12.300s] manifold-core::integration binary::name\n'
            '    PASS [  10.000s] manifold-core boundary\n'
            '    PASS [   9.999s] manifold-core fast\n'
            '    SLOW [>30.000s] manifold-nodes unfinished\n'
            '     Summary [  62.400s] 3 tests run\n'
            '    SLOW [  62.345s] manifold-nodes preset_runtime::tests::foo\n'
        )
        self.assertEqual(landing_gate.parse_slow_tests(transcript), [
            {'name': 'manifold-nodes preset_runtime::tests::foo', 's': 62.345},
            {'name': 'manifold-nodes unfinished', 's': 30.0},
            {'name': 'manifold-core::integration binary::name', 's': 12.3},
            {'name': 'manifold-core boundary', 's': 10.0},
        ])

    def test_caps_at_ten_slowest_with_stable_ties(self):
        transcript = '\n'.join(f'PASS [{n}s] pkg test_{n}' for n in range(25))
        transcript += '\nPASS [24s] pkg a_tie\nPASS [24s] pkg test_24'
        expected = [{'name': 'pkg a_tie', 's': 24.0}] + [
            {'name': f'pkg test_{n}', 's': float(n)} for n in range(24, 15, -1)]
        self.assertEqual(landing_gate.parse_slow_tests(transcript), expected)

    def test_parse_misses_never_raise(self):
        for output in (None, b'PASS [10s] pkg test', '', 'test foo ... ok',
                       'PASS [nans] pkg foo', 'PASS [no duration] pkg foo',
                       'SLOW [>30s]', 'PASS [12.3.4s] pkg foo',
                       'PASS [' + '9' * 400 + 's] pkg foo'):
            with self.subTest(output=output):
                self.assertEqual(landing_gate.parse_slow_tests(output), [])


class LandingTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(landing_gate.gpu_scope, "learned_times_path", return_value=None))

    checks = ["tooling", "design-status", "ignored-tests", "deny",
              "clippy", "tests-build", "test-ownership/manifold-gpu",
              "gpu-proofs-build", "tests", "flow-gate", "gpu-proofs"]

    def exercise(self, failed=None, extra=(), stale_docs=False, packages=True, head="head", paths=None,
                 comment=False, gpu_output=None, proof_cached=False, manifest=None,
                 nextest_output=None, readiness_errors=(), failure_code=1, test_inventory=None):
        called, commands = [], []
        self.events = events = []
        paths = paths or ["crates/manifold-gpu/src/metal/device.rs"]
        if not packages:
            paths = []
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
            if cmd[:3] == ["cargo", "nextest", "list"]:
                ownership_label = f"test-ownership/{cmd[cmd.index('-p') + 1]}"
                called.append(ownership_label)
                testcases = {"tests::test", "metal::device::test",
                             "regenerates_in_sync", "uniform_layout_proof::test",
                             "uniform_layout_extended::test", "wgsl_validation::test"}
                for selected in paths:
                    if "/src/" in selected and selected.endswith(".rs"):
                        module = selected.split("/src/", 1)[1][:-3].replace("/", "::")
                        if module.endswith(("::mod", "::lib")):
                            module = module.rsplit("::", 1)[0]
                        testcases.add(module + "::test")
                    elif "/tests/" in selected:
                        testcases.add("test")
                suites = {name: {"binary-name": name, "testcases": sorted(testcases)}
                          for name in ("synthetic", "main", "gpu_proofs", "glb_conformance",
                                       "uniform_layout_proof", "uniform_layout_extended",
                                       "godfile_regrowth", "no_bespoke_row_infra",
                                       "file_loader_exhaustiveness")}
                return (1 if failed == ownership_label else 0), json.dumps(
                    test_inventory if test_inventory is not None else {"rust-suites": suites}), "", 0.01
            label = ({"nextest": "tests"}.get(cmd[1], cmd[1]) if cmd[0] == "cargo"
                     else labels[Path(cmd[1]).name])
            if "test(regenerates_in_sync)" in cmd:
                label = "catalog-fresh"
            if "--no-run" in cmd or "--build-only" in cmd:
                label += "-build"
            called.append(label)
            events.append(label)
            if label == "gpu-proofs" and gpu_output is not None:
                return (failure_code if label in ([failed] if isinstance(failed, str) else failed or []) else 0), gpu_output, "", 0.01
            if cmd[:2] == ["cargo", "nextest"] and "--no-run" not in cmd and nextest_output:
                return (failure_code if label in ([failed] if isinstance(failed, str) else failed or []) else 0), *nextest_output, 0.01
            return (failure_code if label in ([failed] if isinstance(failed, str) else failed or []) else 0), f"output for {label}\n", "", 0.01

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            root = Path(d)
            workspace = synthetic_workspace(root, paths) if paths else None
            if stale_docs:
                (root / "docs").mkdir()
                (root / "docs/README.md").write_text("stale")
                (root / "docs/A.md").write_text(
                    "# A\n\nA long enough summary line for the index here.\n")
            if manifest is not None:
                (root / "scripts/ui-flows").mkdir(parents=True)
                (root / "scripts/ui-flows/manifest.json").write_text(json.dumps(manifest))
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["landing_gate.py", "--repo", d, *extra]))
            stack.enter_context(patch.object(landing_gate, "MAIN_CHECKOUT", root))
            stack.enter_context(patch.object(landing_gate, "run_cmd", side_effect=run))
            def readiness(_repo, selected_paths, _base):
                from codex_checks import tooling_checks
                tooling = tooling_checks(root, selected_paths)
                if not selected_paths:
                    return {"packages": [], "dependents": [], "cpu": cpu_scope.Plan(),
                            "gpu": gpu_scope.Plan(), "workspace": None, "errors": [], "flows": [], "tooling": tooling}
                cpu = cpu_scope.plan_for_paths(selected_paths, root, workspace=workspace)
                for selected in selected_paths:
                    if "/tests/" in selected and workspace.owner(selected):
                        package = workspace.owner(selected)
                        cpu.packages.add(package)
                        binary = Path(selected).parts[3]
                        if binary.endswith(".rs"):
                            binary = Path(binary).stem
                        cpu.filters.add(f"(package(={package}) & binary(={binary}))")
                gpu = gpu_scope.plan_for_paths(selected_paths, root, workspace=workspace,
                                               shader_users=lambda _path: [])
                errors = list(readiness_errors)
                if gpu.unmapped:
                    errors.append(("gpu-ownership", gpu_scope.unmapped_message(gpu)))
                return {"packages": sorted(cpu.packages), "dependents": [], "cpu": cpu,
                        "gpu": gpu, "workspace": workspace, "errors": errors,
                        "flows": landing_gate.flow_filters(root, selected_paths), "tooling": tooling}
            stack.enter_context(patch.object(landing_gate.gate_readiness, "plan",
                                             side_effect=readiness))
            stack.enter_context(patch.object(landing_gate.gpu_queue, "landing_pending",
                                             return_value=contextlib.nullcontext()))
            # This harness has no Git repository; freshness has dedicated real-Git tests.
            stack.enter_context(patch.object(landing_gate.gate_passes, 'changed_passes', return_value=[]))
            if proof_cached:
                from unittest.mock import Mock
                stack.enter_context(patch.object(landing_gate.gate_passes, "proof_pass",
                    return_value=Mock(record={"seconds": 400})))
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

    def test_nextest_timings_keep_full_output_on_pass_and_failure(self):
        stdout = 'PASS [12.3s] manifold-nodes stdout_test'
        stderr = 'SLOW [>30s] manifold-nodes stderr_test\n' + 'noise\n' * 25
        expected = [{'name': 'manifold-nodes stderr_test', 's': 30.0},
                    {'name': 'manifold-nodes stdout_test', 's': 12.3}]
        for failed in (None, 'tests'):
            with self.subTest(failed=failed):
                _, _, timings, _, _, _, _ = self.exercise(
                    failed=failed, nextest_output=(stdout, stderr), gpu_output=stdout,
                    paths=['crates/manifold-nodes-scene/src/node_graph/primitives/camera_lens.rs'])
                for check in timings['checks']:
                    if check['label'].startswith('tests/'):
                        self.assertEqual(check['slow_tests'], expected)
                    elif check['label'] == 'tests-build' or check['label'].startswith('tests-build/'):
                        self.assertEqual(check['slow_tests'], [])
                    else:
                        self.assertNotIn('slow_tests', check)
        _, _, timings, *_ = self.exercise()
        self.assertEqual(next(c for c in timings['checks'] if c['label'].startswith('tests/'))['slow_tests'], [])
        self.assertIsNone(landing_gate.SLOW_TESTS.get())

    def test_every_failure_stops_later_checks_and_retains_evidence(self):
        failure_cases = [(index, failed) for index, failed in enumerate(self.checks)
                         if not failed.startswith("test-ownership/")]
        for index, failed in failure_cases:
            with self.subTest(failed=failed):
                code, called, timings, commands, logs, output, deps = self.exercise(
                    failed, extra=["--fail-fast"])
                self.assertEqual(code, 1)
                expected = self.checks[:index + 1]
                self.assertEqual(called, expected)
                self.assertEqual(timings["failed"], 1)
                if index >= 3:
                    self.assertEqual(timings["checks"][-1]["status"], "FAIL")
                if failed.startswith("test-ownership/"):
                    self.assertTrue(logs)
                else:
                    self.assertTrue(any(f"output for {failed}" in log for log in logs))
                self.assertIn(f"[RUN] {failed}", output)
                self.assertNotIn(["git", "log", "base..HEAD", "--format=%B"], commands)
                if index < 5:
                    self.assertEqual(deps, 0)

    def test_keep_going_collects_every_red_with_a_rerun_command_each(self):
        # The gate is for landing; reds are fixed with the printed commands,
        # never by rerunning the gate to find the next one.
        code, called, timings, _, _, output, _ = self.exercise("clippy", extra=["--keep-going"])
        self.assertEqual(code, landing_gate.CHECKS_RED)
        self.assertEqual(called, self.checks)
        self.assertEqual(timings["failed"], 1)
        summary = output[output.index("FAIL clippy"):]
        self.assertRegex(summary, r"rerun: cargo clippy --manifest-path \S+/Cargo.toml -p manifold-gpu")
        self.assertIn("fix each red with its `rerun:` command", summary)

    def test_default_collects_cheap_reds_and_stops_expensive_legs(self):
        code, called, timings, _, _, output, _ = self.exercise(
            failed=['deny', 'clippy', 'tests'],
            readiness_errors=[('entrypoints', 'missing executable')])
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks[:-3])
        self.assertEqual(timings['failed'], 3)
        self.assertFalse(any(event.startswith('hold-enter:') for event in self.events))
        self.assertIn('flows and GPU proofs were not run because', output)
        for label in ('entrypoints', 'deny', 'clippy'):
            self.assertIn('FAIL ' + label, output)
        self.assertGreaterEqual(output.count('rerun: '), 6)

    def test_terminal_success_follows_verdict_publication(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            root = Path(directory)
            (root / 'scripts').mkdir()
            (root / 'scripts/gate_runner.py').write_text(
                'def append_verdict(*args): print("publication completed")\n')
            stack.enter_context(patch.object(landing_gate, 'MAIN_CHECKOUT', root))
            stack.enter_context(patch.object(landing_gate, 'run_cmd',
                return_value=(0, 'BUG-fake', '', 0)))
            for changes in (([], []), ([], ['changed-input'])):
                with patch.object(landing_gate.gate_passes, 'changed_passes', side_effect=changes), \
                        contextlib.redirect_stdout(io.StringIO()) as output:
                    code = landing_gate.finish(root, 'base', [('PASS', 'fake', 1, [])])
                text = output.getvalue()
                self.assertIn('landing gate: 1 passed, 0 failed', text)
                if code == 0:
                    self.assertLess(text.index('publication completed'), text.index('[COMPLETE]'))
                else:
                    self.assertIn('[FAIL] inputs changed', text)
                    self.assertNotIn('[COMPLETE]', text)

    def test_planning_fail_fast_stops_before_any_leg(self):
        code, called, *_ = self.exercise(extra=['--fail-fast'],
            readiness_errors=[('entrypoints', 'missing executable'), ('references', 'missing include')])
        self.assertEqual(code, 1)
        self.assertEqual(called, [])

    def test_test_failure_precedes_flows_even_in_diagnostic_mode(self):
        code, called, *_ = self.exercise('tests', extra=['--keep-going'])
        self.assertEqual(code, landing_gate.CHECKS_RED)
        self.assertLess(called.index('tests'), called.index('flow-gate'))

    def test_quoted_proof_refusal_is_an_ordinary_failure(self):
        output = 'AssertionError: "GPU-PROOFS GATE: FAIL (inputs changed"\n'
        for label in ("tests", "gpu-proofs"):
            with self.subTest(label=label):
                code, *_ = self.exercise(
                    failed=label, gpu_output=output, nextest_output=(output, ""), extra=["--keep-going"])
                self.assertEqual(code, landing_gate.CHECKS_RED)

    def test_proof_refusal_status_is_only_reserved_for_proof_subprocess(self):
        for label in ("tests", "gpu-proofs", "gpu-proofs-build"):
            with self.subTest(label=label):
                code, *_ = self.exercise(
                    failed=label, failure_code=landing_gate.PROOF_INPUTS_CHANGED, extra=["--keep-going"])
                self.assertEqual(code, landing_gate.CHECKS_RED if label == "tests" else 1)

    def test_leg_rerun_lines_win_over_the_leg_command(self):
        gpu_output = ("failures:\n    liquid_conformance::broken\n\n"
                      "test result: FAILED. 0 passed; 1 failed;\n"
                      "rerun: /r/scripts/gpu_proofs_gate.py --filter liquid_conformance::broken\n"
                      "GPU-PROOFS GATE: FAIL (1 failed tests, 0 drifted goldens)\n")
        _, _, _, _, _, output, _ = self.exercise(failed="gpu-proofs", gpu_output=gpu_output)
        summary = output[output.index("FAIL gpu-proofs"):]
        self.assertIn("rerun: /r/scripts/gpu_proofs_gate.py --filter liquid_conformance::broken", summary)
        self.assertNotIn("rerun: /", summary.replace("rerun: /r/scripts", ""))

    def test_flow_gate_builds_before_the_hold_and_runs_under_it(self):
        path = "crates/manifold-ui/src/panels/inspector.rs"
        manifest = {"flows": {"inspector-scroll": "inspector"},
                    "path_triggers": {"crates/manifold-ui/src/panels/": ["inspector"]}}
        code, called, timings, commands, *_ = self.exercise(paths=[path], manifest=manifest)
        self.assertEqual(code, 0)
        events = self.events
        enter = events.index("hold-enter:landing_gate flows+tests+gpu-proofs")
        self.assertLess(events.index("flow-gate-build"), enter)
        self.assertLess(enter, events.index("flow-gate"))
        self.assertIn(["python3", "scripts/run_ui_flows.py", "--touched", "base...HEAD", "--build-only"], commands)
        self.assertEqual(timings["gpu_wait_s"], 0.0)

    def test_stale_docs_stop_before_cargo_or_rendering(self):
        code, called, timings, *_ = self.exercise(stale_docs=True, extra=["--fail-fast"])
        self.assertEqual(code, 1)
        self.assertEqual(called, [])
        self.assertEqual(timings["checks"][-1]["label"], "fresh-docs-index")

    def test_readiness_collects_all_cheap_failures_before_runtime(self):
        errors = (("metadata", "cargo metadata failed"),
                  ("references", "missing include assets/fixture.bin"))
        code, called, timings, commands, _, output, _ = self.exercise(
            readiness_errors=errors)
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks[:-3])
        self.assertEqual([row["label"] for row in timings["checks"]][:2],
                         ["metadata", "references"])
        self.assertIn("[FAIL] metadata", output)
        self.assertIn("[FAIL] references", output)
        reruns = [line.strip() for line in output.splitlines() if line.strip().startswith('rerun: ')]
        self.assertEqual(len(reruns), 4)  # Immediate diagnostics and final summary.
        self.assertTrue(all('landing_gate.py --repo ' in line and '--plan-only' in line
                            for line in reruns))
        self.assertNotIn("tests", called)
        self.assertNotIn("flow-gate", called)
        self.assertNotIn("gpu-proofs", called)

    def test_testless_source_module_runs_whole_package_after_inventory(self):
        listing = {"rust-suites": {
            "gpu": {"binary-name": "manifold_gpu", "testcases": ["other::case"]},
        }}
        code, called, _, commands, _, output, _ = self.exercise(test_inventory=listing)
        self.assertEqual(code, 0)
        self.assertIn("tests", called)
        runs = [cmd for cmd in commands if cmd[:3] == ["cargo", "nextest", "run"]
                and "--no-run" not in cmd]
        self.assertEqual(len(runs), 1)
        self.assertEqual(runs[0][runs[0].index("-E") + 1], "package(=manifold-gpu)")
        self.assertIn("device.rs has no tests of its own: running manifold-gpu whole", output)
        self.assertNotIn("[SKIP] tests/manifold-gpu", output)

    def test_empty_ownership_scope_is_red_before_runtime(self):
        code, called, _, commands, _, output, _ = self.exercise(
            readiness_errors=(("cpu-ownership",
                               "manifold-gpu: ownership mapping resolves to no tests"),))
        self.assertEqual(code, 1)
        self.assertEqual(called, self.checks[:-3])
        self.assertIn("[FAIL] cpu-ownership", output)
        self.assertIn("resolves to no tests", output)
        self.assertNotIn("tests", called)
        self.assertNotIn("flow-gate", called)
        self.assertNotIn("gpu-proofs", called)

    def test_real_gpu_failure_over_budget_names_failure_and_deferred_in_finish(self):
        import gpu_proofs_gate
        summary = io.StringIO()
        with contextlib.redirect_stdout(summary):
            verdict = gpu_proofs_gate.print_summary(
                "failures:\n    liquid_conformance::broken\n\ntest result: FAILED. 0 passed; 1 failed;\n",
                101, [("liquid_conformance::broken", 401, "b", True)], 360)
        self.assertEqual(verdict, 101)
        deferred = "GPU-PROOFS DEFERRED: liquid_conformance::other (100s)"
        code, _, _, _, _, output, _ = self.exercise(
            failed="gpu-proofs", gpu_output=deferred + "\n" + summary.getvalue())
        self.assertEqual(code, landing_gate.CHECKS_RED)
        self.assertIn("GPU-PROOFS BUDGET: OVER", output)
        self.assertIn("liquid_conformance::broken", output)
        self.assertIn("GPU-PROOFS GATE: FAIL", output)
        self.assertGreaterEqual(output.count(deferred), 2)

    def test_reused_proofs_keep_deferred_line_in_finish(self):
        path = landing_gate.gpu_scope.PROOFS_DIR + "liquid_conformance.rs"
        with patch.object(landing_gate.gpu_scope, "load_times", return_value={
                "liquid_conformance::slow": 100, "unrelated::slow": 200}):
            code, called, _, _, _, output, _ = self.exercise(paths=[path], proof_cached=True)
        self.assertEqual(code, 0)
        self.assertNotIn("gpu-proofs", called)
        self.assertIn("REUSED gpu-proofs", output)
        self.assertNotIn("unrelated::slow", output)
        summary = output[output.index("REUSED gpu-proofs"):]
        self.assertNotIn("GPU-PROOFS DEFERRED:", summary)

    def test_success_runs_all_required_checks_with_explicit_gpu_binary(self):
        code, called, timings, commands, _, output, _ = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(called, self.checks)
        self.assertEqual(timings["failed"], 0)
        self.assertIn(["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs",
                       "--budget", "360", "--learn-times"], commands)
        self.assertTrue(all("--all" not in c and "--full-suite" not in c for c in commands))
        self.assertIn("[gpu-proofs] mode: scoped", output)
        self.assertIn("GPU backend core", output)

    def test_tests_and_gpu_proofs_legs_run_inside_one_hold(self):
        code, *_ = self.exercise()
        self.assertEqual(code, 0)
        events = self.events
        self.assertEqual([e for e in events if e.startswith("hold")],
                         ["hold-enter:landing_gate flows+tests+gpu-proofs", "hold-exit"])
        enter, leave = events.index("hold-enter:landing_gate flows+tests+gpu-proofs"), events.index("hold-exit")
        self.assertLess(enter, events.index("tests"))
        self.assertLess(events.index("gpu-proofs"), leave)
        # Cheap prerequisites, clippy and every test-binary compile run
        # before the hold, so it covers test time only.
        self.assertLess(events.index("clippy"), enter)
        self.assertLess(events.index("tests-build"), enter)
        self.assertLess(events.index("gpu-proofs-build"), enter)

    def test_builds_compile_exactly_what_the_held_legs_run(self):
        _, _, _, commands, *_ = self.exercise()
        nextest = [c for c in commands if c[:2] == ["cargo", "nextest"] and c[2] != "list"]
        selection = ["-p", "manifold-gpu",
                     "-E", "(package(=manifold-gpu) & test(/^metal::device::/))"]
        self.assertEqual(nextest, [["cargo", "nextest", "run", "--no-run", *selection],
                                   ["cargo", "nextest", "run", "--no-fail-fast", *selection]])
        proofs = [c for c in commands if c[1:2] == ["scripts/gpu_proofs_gate.py"]]
        self.assertEqual(proofs, [
            ["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs", "--build-only"],
            ["python3", "scripts/gpu_proofs_gate.py", "--path", "crates/manifold-gpu/src/metal/device.rs", "--budget", "360", "--learn-times"]])

    def test_catalog_check_skipped_when_renderer_untouched(self):
        _, _, _, commands, *_ = self.exercise()
        self.assertFalse(any("test(regenerates_in_sync)" in c for c in commands))

    def test_proofs_do_not_change_nextest_selection(self):
        # Cover nested gpu_tests, scene modules, individually gated tests,
        # required-features binaries, and the transitive manifold-gpu feature.
        # Compare entire argv: no feature, package or filter changes may leak
        # from proof selection into nextest (including build/catalog commands).
        for path in (
            "crates/manifold-nodes-image/src/node_graph/primitives/invert.rs",
            "crates/manifold-nodes-image/src/node_graph/primitives/mod.rs",
            "crates/manifold-node-engine/src/water/primitives/gpu_flip_scene_tests.rs",
            "crates/manifold-nodes/src/bundled_presets.rs",
            "crates/manifold-nodes/tests/gpu_proofs/main.rs",
            "crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs",
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
                     "--budget", "360", "--learn-times"], enabled)

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
            assets = Path(d) / "crates/manifold-nodes/assets"
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
        self.assertEqual(called, self.checks[:-3])
        self.assertIn("rerun: regen-cmd", output)

    def test_build_failure_stops_before_the_hold(self):
        for failed in ("tests-build", "gpu-proofs-build"):
            with self.subTest(failed=failed):
                code, *_ = self.exercise(failed, extra=["--fail-fast"])
                self.assertEqual(code, 1)
                self.assertFalse(any(e.startswith("hold") for e in self.events))

    def test_build_failure_skips_only_the_leg_it_feeds(self):
        for failed, skipped, still_run in (("tests-build", "tests", "gpu-proofs"),
                                           ("gpu-proofs-build", "gpu-proofs", "tests")):
            with self.subTest(failed=failed):
                code, called, _, _, _, output, _ = self.exercise(failed, extra=["--keep-going"])
                self.assertEqual(code, 1)
                self.assertNotIn(skipped, called)
                self.assertIn(still_run, called)
        skipped_label = "tests/manifold-gpu" if skipped == "tests" else skipped
        self.assertIn(f"[SKIP] {skipped_label} ({failed} failed)", output)

    def test_tests_failure_still_releases_the_hold(self):
        self.exercise("tests")
        self.assertEqual(self.events[-1], "hold-exit")

    def test_unmapped_gpu_path_fails_gate_naming_path(self):
        # A non-Rust source file is GPU-scoped but owns no CPU test binary.
        # Putting it under tests/ would make this harness select a CPU binary.
        code, called, _, _, _, output, _ = self.exercise(
            paths=["crates/manifold-node-engine/src/orphan.bin"])
        self.assertEqual(code, 1)
        self.assertNotIn("gpu-proofs", called)
        self.assertNotIn("gpu-proofs-build", called)
        self.assertNotIn("tests-build", called)
        self.assertIn("src/orphan.bin", output)
        self.assertIn("Add a mapping rule", output)

    def test_named_red_collection_does_not_skip_remaining_checks(self):
        code, called, timings, *_ = self.exercise("deny", extra=["--keep-going"])
        self.assertEqual(code, landing_gate.CHECKS_RED)
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
        self.assertIn("[SKIP] clippy (docs/comment-only diff)", output)
        self.assertIn("[SKIP] tests (docs/comment-only diff)", output)
        self.assertIn("[SKIP] gpu-proofs (docs/comment-only diff)", output)
        self.assertIn("SKIP clippy (docs/comment-only diff)\n", output)

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

    def test_missing_coverage_cannot_be_waived_as_named_red(self):
        for code, output in [(-1, 'timeout'),
                             (4, 'GPU-PROOFS GATE: HUNG proof'),
                             (5, 'GPU-PROOFS TIMING: FAIL (missing allowance)')]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as d, \
                    patch.object(landing_gate, 'run_cmd', return_value=(code, output, '', 1)), \
                    contextlib.redirect_stdout(io.StringIO()):
                token = landing_gate.RAN_EVERY_CHECK.set(True)
                try:
                    landing_gate.run_check('proof', ['tool'], Path(d), 60)
                    self.assertFalse(landing_gate.RAN_EVERY_CHECK.get())
                finally:
                    landing_gate.RAN_EVERY_CHECK.reset(token)

    def test_nightly_keeps_full_renderer_coverage(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["trunk_health.py", "--dry-run"]))
            stack.enter_context(patch.object(trunk_health, "LOG_DIR", Path(d)))
            stack.enter_context(patch.object(trunk_health, "BD", "bd"))
            commands = stack.enter_context(patch.object(trunk_health, "run_cmd", return_value=(0, "tip", "", 0)))
            stack.enter_context(patch.object(trunk_health, "cap_main_target", return_value=""))
            hold = stack.enter_context(patch.object(trunk_health.gpu_queue, "hold"))
            stack.enter_context(patch.object(trunk_health.subprocess, "run",
                                            return_value=subprocess.CompletedProcess([], 0, "", "")))
            self.assertEqual(trunk_health.main(), 0)
            hold.assert_not_called()
            self.assertEqual([c.args[0] for c in commands.call_args_list],
                             [["git", "rev-parse", "--short=12", "origin/main"]])
        self.assertIn("would run: python3 scripts/gpu_proofs_gate.py --all", output.getvalue())
        self.assertIn("would run: cargo nextest run --workspace --no-fail-fast", output.getvalue())

    def test_comment_only_rust_skips_builds_tests_and_gpu_without_hold(self):
        code, called, _, commands, _, output, deps = self.exercise(comment=True)
        self.assertEqual(code, 0)
        for label in ("clippy", "tests-build", "gpu-proofs-build", "tests", "gpu-proofs"):
            self.assertNotIn(label, called)
            self.assertIn(f"[SKIP] {label} (docs/comment-only diff)", output)
        self.assertFalse(any(e.startswith("hold") for e in self.events))
        self.assertFalse(any(c[:2] == ["cargo", "nextest"] for c in commands))
        self.assertEqual(deps, 0)

    def test_one_primitive_selects_its_module_and_the_layout_proofs(self):
        path = "crates/manifold-nodes-scene/src/node_graph/primitives/camera_lens.rs"
        _, _, _, commands, _, output, _ = self.exercise(paths=[path])
        expected = sorted([
            "(package(=manifold-nodes-scene) & test(/^node_graph::primitives::camera_lens::/))",
            "(package(=manifold-nodes) & test(/^uniform_layout_proof::/))",
            "(package(=manifold-nodes) & test(/^uniform_layout_extended::/))",
            "(package(=manifold-nodes) & test(regenerates_in_sync))",
        ])
        scoped = [c for c in commands if c[:2] == ["cargo", "nextest"] and c[2] == "run"
                  and "-E" in c]
        builds = [c[c.index("-E") + 1] for c in scoped if "--no-run" in c]
        runs = sorted(c[c.index("-E") + 1] for c in scoped if "--no-run" not in c)
        selections = sorted([" | ".join(f for f in expected if "package(=manifold-nodes)" in f),
                             " | ".join(f for f in expected if "package(=manifold-nodes-scene)" in f)])
        self.assertEqual(sorted(builds), selections)
        self.assertEqual(runs, selections)
        self.assertIn("[tests] filterset: " + " | ".join(expected), output)


class NightlyQueueTests(unittest.TestCase):
    def test_gpu_legs_yield_between_checks_and_use_nightly_priority(self):
        held = False
        events = []

        @contextlib.contextmanager
        def hold(label, priority="normal"):
            nonlocal held
            self.assertEqual(label, "trunk_health gpu legs")
            self.assertEqual(priority, "nightly")
            events.append("acquire")
            held = True
            try:
                yield
            finally:
                held = False
                events.append("release")

        def run(cmd, cwd, timeout):
            if cmd[0] == "git" or "scripts/hook_census.py" in cmd:
                self.assertFalse(held)
                return 0, "tip", "", 0
            self.assertEqual(timeout, 5400)
            events.append((cmd, held))
            if "nextest" in cmd:
                self.assertEqual(held, "--no-run" not in cmd)
            if "scripts/gpu_proofs_gate.py" in cmd:
                raise subprocess.TimeoutExpired(cmd, timeout)
            return (1 if "nextest" in cmd and "--no-run" not in cmd else 0), "", "", 0

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(sys, "argv", ["trunk_health.py"]))
            stack.enter_context(patch.object(trunk_health, "LOG_DIR", Path(d)))
            stack.enter_context(patch.object(trunk_health, "missing_tools", return_value=[]))
            stack.enter_context(patch.object(trunk_health, "cap_main_target", return_value=""))
            stack.enter_context(patch.object(trunk_health, "run_cmd", side_effect=run))
            stack.enter_context(patch.object(trunk_health.gpu_queue, "hold", side_effect=hold))
            beads = stack.enter_context(patch.object(trunk_health.subprocess, "run",
                return_value=subprocess.CompletedProcess([], 0, "[]", "")))
            self.assertEqual(trunk_health.main(), 1)
            self.assertEqual(sum("create" in c.args[0] for c in beads.call_args_list), 2)
        self.assertEqual(events.count("acquire"), 5)
        self.assertEqual(events.count("release"), 5)
        cpu = events[:7]
        gpu = [event for event in events if isinstance(event, tuple) and event[1]]
        self.assertTrue(all(not locked for _, locked in cpu))
        self.assertEqual(cpu[3][0], ["cargo", "clippy", "--workspace", "--tests", "--", "-D", "warnings"])
        self.assertEqual(cpu[4][0], ["cargo", "nextest", "run", "--workspace", "--no-fail-fast", "--no-run"])
        self.assertEqual([cmd[1] for cmd, _ in gpu], ["nextest", "scripts/gpu_proofs_gate.py",
            "scripts/rt_noise_gate.py", "scripts/rt_noise_gate.py", "scripts/bridge_probe_gate.py"])
        self.assertTrue(all(locked for _, locked in gpu))
        self.assertTrue(all(event[1] for event in events
                            if isinstance(event, tuple)
                            and "nextest" in event[0] and "--no-run" not in event[0]))


class BridgeProbeQueueTests(unittest.TestCase):
    def test_busy_gpu_polls_until_clear(self):
        output = io.StringIO()
        with patch.object(bridge_probe_gate, "_gpu_processes", side_effect=[
                [("123", "manifold bridge-probe")], [("456", "manifold --capture")], []]), \
                patch.object(bridge_probe_gate.time, "monotonic", side_effect=[0, 0, 15]), \
                patch.object(bridge_probe_gate.time, "sleep") as sleep, \
                contextlib.redirect_stdout(output):
            self.assertTrue(bridge_probe_gate.check_gpu_busy())
        self.assertEqual([c.args for c in sleep.call_args_list], [(15,), (15,)])
        self.assertIn("PID 123: manifold bridge-probe", output.getvalue())
        self.assertIn("PID 456: manifold --capture", output.getvalue())

    def test_busy_gpu_times_out_after_twenty_minutes(self):
        now = 0

        def sleep(seconds):
            nonlocal now
            self.assertEqual(seconds, 15)
            now += seconds

        output = io.StringIO()
        with patch.object(bridge_probe_gate, "_gpu_processes", return_value=[("123", "manifold --capture")]), \
                patch.object(bridge_probe_gate.time, "monotonic", side_effect=lambda: now), \
                patch.object(bridge_probe_gate.time, "sleep", side_effect=sleep), \
                contextlib.redirect_stdout(output):
            self.assertFalse(bridge_probe_gate.check_gpu_busy())
        self.assertEqual(now, 1200)
        self.assertIn("EXIT 2", output.getvalue())

    def test_idle_gpu_does_not_sleep(self):
        with patch.object(bridge_probe_gate, "_gpu_processes", return_value=[]), \
                patch.object(bridge_probe_gate.time, "sleep") as sleep:
            self.assertTrue(bridge_probe_gate.check_gpu_busy())
            sleep.assert_not_called()

    def test_process_list_reports_pid_and_filters_non_gpu_work(self):
        listing = "123 /tmp/manifold bridge-probe\n456 cargo test --features gpu-proofs\n789 unrelated\n"
        with patch.object(bridge_probe_gate.subprocess, "run",
                return_value=subprocess.CompletedProcess([], 0, listing, "")) as run:
            self.assertEqual(bridge_probe_gate._gpu_processes(), [
                ("123", "/tmp/manifold bridge-probe"), ("456", "cargo test --features gpu-proofs")])
        self.assertEqual(run.call_args.args[0], ["ps", "-ax", "-o", "pid=,command="])

    def test_entrypoint_checks_gpu_inside_hold_and_returns_two_on_timeout(self):
        held = False
        now = 0

        @contextlib.contextmanager
        def hold(label):
            nonlocal held
            self.assertEqual(label, "bridge_probe_gate")
            held = True
            try:
                yield
            finally:
                held = False

        def run(cmd, **kwargs):
            self.assertTrue(held)
            self.assertEqual(cmd[0], "ps")
            return subprocess.CompletedProcess(cmd, 0, "123 manifold --capture\n", "")

        def sleep(seconds):
            nonlocal now
            now += seconds

        with patch.object(sys, "argv", ["bridge_probe_gate.py"]), \
                patch.object(trunk_health.gpu_queue, "hold", side_effect=hold), \
                patch.object(subprocess, "run", side_effect=run), \
                patch.object(time, "monotonic", side_effect=lambda: now), \
                patch.object(time, "sleep", side_effect=sleep), \
                contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit) as exited:
            runpy.run_path(str(Path(bridge_probe_gate.__file__)), run_name="__main__")
        self.assertEqual(exited.exception.code, 2)
        self.assertFalse(held)


class DiffScopeTests(unittest.TestCase):
    def test_manifests_select_workspace_layering_contract(self):
        for path in ("Cargo.toml", "crates/manifold-ui-paint/Cargo.toml"):
            with self.subTest(path=path):
                with tempfile.TemporaryDirectory() as d:
                    plan = cpu_scope.plan_for_paths(
                        [path], Path(d), workspace=synthetic_workspace(d, [path]))
                self.assertIn("(package(=manifold-app) & binary(=crate_layering))", plan.filters)

    def test_ceiling_paths_select_app_godfile_binary_across_packages(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-core"
            (crate / "src/effects").mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-core"\n')
            workspace = synthetic_workspace(d, ["crates/manifold-core/src/effects/instance.rs"])
            plan = cpu_scope.plan_for_paths(["crates/manifold-core/src/effects/instance.rs"], d,
                                            workspace=workspace)
            self.assertEqual(plan.packages, {"manifold-core", "manifold-app"})
            self.assertIn("(package(=manifold-core) & test(/^effects::instance::/))", plan.filters)
            self.assertIn("(package(=manifold-app) & binary(=godfile_regrowth))", plan.filters)
            for path in cpu_scope.godfile_paths():
                with self.subTest(path=path):
                    owner_workspace = synthetic_workspace(d, [path])
                    plan = cpu_scope.plan_for_paths([path], d, workspace=owner_workspace)
                    self.assertIn("(package(=manifold-app) & binary(=godfile_regrowth))", plan.filters)

    def test_ceiling_parser_rejects_missing_empty_or_unparsed_tables(self):
        for text in ("", "const CEILINGS: &[(&str, usize)] = &[];",
                     'const CEILINGS: &[(&str, usize)] = &[("a.rs", 100), unknown];'):
            with self.subTest(text=text), patch.object(Path, "read_text", return_value=text):
                with self.assertRaisesRegex(ValueError, "cannot parse CEILINGS"):
                    cpu_scope.godfile_paths()

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

    def test_plain_spans_stop_at_every_lexer_state_transition(self):
        for source, expected in [
            ('prefix // hidden\nnext', ['prefix ', 'next']),
            ('prefix r#"// literal"# suffix', ['prefix r#"// literal"# suffix']),
            ('prefix br"/* literal */" suffix', ['prefix br"/* literal */" suffix']),
            ('prefix cr##"// literal"## suffix', ['prefix cr##"// literal"## suffix']),
            ("prefix 'a' suffix", ["prefix 'a' suffix"]),
            ('prefix "/* literal */" suffix', ['prefix "/* literal */" suffix']),
        ]:
            with self.subTest(source=source):
                self.assertEqual(diff_scope.code_lines(source, '.rs'), expected)

    def test_changed_lines_drive_path_exclusion(self):
        path = "crates/manifold-nodes-image/src/node_graph/primitives/bilateral_blur.rs"
        old_oid, new_oid = "a" * 40, "b" * 40
        raw = (f":100644 100644 {old_oid} {new_oid} M\0{path}\0\0"
               f"diff --git a/{path} b/{path}\n@@ -1 +1 @@\n-// old\n+// new\n")

        def git(repo, *args):
            self.assertEqual(args[0], "diff")
            return raw

        with patch.object(diff_scope, "git", side_effect=git), \
                patch.object(diff_scope, "_cat_file_batch",
                             return_value={old_oid: "// old\nfn f() {}\n",
                                            new_oid: "// new\nfn f() {}\n"}):
            self.assertEqual(diff_scope.effective_paths(Path.cwd(), "base"), ([], [path]))

    def test_raw_patch_batch_handles_nul_safe_paths_statuses_and_modes(self):
        comment_path = "src/odd name\t.rs"
        code_path = "src/line\nbreak.rs"
        mode_path = "src/mode.rs"
        deleted_path = "src/deleted.rs"
        docs_path = "README.md"
        zero = "0" * 40
        objects = {
            "a" * 40: "// old\nfn f() {}\n",
            "b" * 40: "// new\nfn f() {}\n",
            "c" * 40: "fn old() {}\n",
            "d" * 40: "fn new() {}\n",
            "e" * 40: "// mode old\n",
            "f" * 40: "// mode new\n",
            "1" * 40: "// deleted\n",
            "2" * 40: "docs old\n",
            "3" * 40: "docs new\n",
        }

        def raw_record(old_mode, new_mode, old_oid, new_oid, status, path):
            return f":{old_mode} {new_mode} {old_oid} {new_oid} {status}\0{path}\0"

        records = "".join([
            raw_record("100644", "100644", "a" * 40, "b" * 40, "M", comment_path),
            raw_record("100644", "100644", "c" * 40, "d" * 40, "M", code_path),
            raw_record("100644", "100755", "e" * 40, "f" * 40, "M", mode_path),
            raw_record("100644", "000000", "1" * 40, zero, "D", deleted_path),
            raw_record("100644", "100644", "2" * 40, "3" * 40, "M", docs_path),
        ])
        patches = "".join([
            f"diff --git a/{comment_path} b/{comment_path}\n@@ -1 +1 @@\n-// old\n+// new\n",
            f"diff --git a/{code_path} b/{code_path}\n@@ -1 +1 @@\n-fn old() {{}}\n+fn new() {{}}\n",
            f"diff --git a/{mode_path} b/{mode_path}\nold mode 100644\nnew mode 100755\n",
            f"diff --git a/{deleted_path} b/{deleted_path}\n@@ -1 +0,0 @@\n-// deleted\n",
            f"diff --git a/{docs_path} b/{docs_path}\n@@ -1 +1 @@\n-docs old\n+docs new\n",
        ])
        calls = []

        def git(repo, *args):
            calls.append(args)
            return records + "\0" + patches

        def cat_file_batch(repo, object_ids):
            self.assertEqual(object_ids, [
                "a" * 40, "b" * 40, "c" * 40, "d" * 40,
                "e" * 40, "f" * 40, "1" * 40,
            ])
            return {object_id: objects[object_id] for object_id in object_ids}

        with patch.object(diff_scope, "git", side_effect=git), \
                patch.object(diff_scope, "_cat_file_batch", side_effect=cat_file_batch):
            active, ignored = diff_scope.effective_paths(Path.cwd(), "base")

        self.assertEqual(active, [code_path, mode_path])
        self.assertEqual(ignored, [comment_path, deleted_path, docs_path])
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "diff")
        self.assertIn("--raw", calls[0])
        self.assertIn("--abbrev=40", calls[0])
        self.assertIn("--patch", calls[0])
        self.assertIn("-z", calls[0])
        self.assertNotIn("--name-only", calls[0])

    def test_worktree_comment_edit_normalizes_batched_blob_newlines(self):
        for newline in ("\r\n", "\r"):
            with self.subTest(newline=newline), tempfile.TemporaryDirectory() as d:
                root = Path(d)
                path = "comment.rs"
                before = f"fn f() {{}} /*{newline}old{newline}*/{newline}".encode()
                (root / path).write_bytes(before.replace(b"old", b"new"))
                old_oid = "a" * 40
                raw = (f":100644 100644 {old_oid} {'0' * 40} M\0{path}\0\0"
                       f"diff --git a/{path} b/{path}\n@@ -2 +2 @@\n-old\n+new\n")
                batch = f"{old_oid} blob {len(before)}\n".encode() + before + b"\n"
                with patch.object(diff_scope, "git", return_value=raw), \
                        patch.object(diff_scope.subprocess, "run", return_value=
                                     subprocess.CompletedProcess([], 0, batch, b"")):
                    self.assertEqual(diff_scope.effective_paths(root, "base", head=None),
                                     ([], [path]))

    def test_worktree_head_reads_after_side_from_filesystem(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            path = "src/comment.rs"
            target = root / path
            target.parent.mkdir(parents=True)
            target.write_text("// new\nfn f() {}\n")
            old_oid = "a" * 40
            zero = "0" * 40
            raw = (f":100644 100755 {old_oid} {zero} M\0{path}\0\0"
                   f"diff --git a/{path} b/{path}\nold mode 100644\nnew mode 100755\n"
                   f"@@ -1 +1 @@\n-// old\n+// new\n")
            with patch.object(diff_scope, "git", return_value=raw), \
                    patch.object(diff_scope, "_cat_file_batch",
                                  return_value={old_oid: "// old\nfn f() {}\n"}):
                self.assertEqual(
                    diff_scope.effective_paths(root, "base", head=None),
                    ([path], []),
                )

    def test_sibling_alias_and_integration_mapping(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-node-engine"
            src = crate / "src/water"
            src.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-node-engine"\n')
            (src / "fluid.rs").write_text('#[path = "fluid_tests.rs"]\nmod checks;\n')
            (src / "fluid_tests.rs").write_text("")
            workspace = synthetic_workspace(d, ["crates/manifold-node-engine/src/water/fluid.rs"])
            plan = cpu_scope.plan_for_paths(["crates/manifold-node-engine/src/water/fluid.rs"], d,
                                            workspace=workspace)
            self.assertIn("test(/^water::fluid::checks::/)", plan.filterset)
            self.assertIn("binary(=gpu_proofs)", plan.filterset)

    def test_gpu_proofs_only_module_gets_no_cpu_filter(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-ui-paint"
            src = crate / "src"
            src.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-ui-paint"\n')
            (src / "ui_renderer.rs").write_text(
                '#[cfg(all(test, feature = "gpu-proofs"))]\nmod tests {}\n')
            (src / "native_text.rs").write_text('#[cfg(test)]\nmod tests {}\n')
            paths = ["crates/manifold-ui-paint/src/ui_renderer.rs",
                     "crates/manifold-ui-paint/src/native_text.rs"]
            workspace = synthetic_workspace(d, paths)
            plan = cpu_scope.plan_for_paths(paths, d, workspace=workspace)
            self.assertNotIn("test(/^ui_renderer::/)", plan.filterset)
            self.assertIn("test(/^native_text::/)", plan.filterset)

    def test_deleted_integration_test_selects_no_binary(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-nodes"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-nodes"\n')
            workspace = synthetic_workspace(d, ["crates/manifold-nodes/tests/fluid_preset.rs"])
            (Path(d) / "crates/manifold-nodes/tests/fluid_preset.rs").unlink()
            plan = cpu_scope.plan_for_paths(
                ["crates/manifold-nodes/tests/fluid_preset.rs"], d, workspace=workspace)
            self.assertEqual(plan.filters, set())
            self.assertEqual(plan.packages, set())

    def test_deleted_bin_target_selects_no_binary(self):
        with tempfile.TemporaryDirectory() as d:
            path = "crates/manifold-nodes/src/bin/graph_tool.rs"
            workspace = synthetic_workspace(d, [path])
            (Path(d) / path).unlink()
            plan = cpu_scope.plan_for_paths([path], d, workspace=workspace)
            self.assertEqual(plan.filters, set())
            self.assertEqual(plan.packages, set())

    def test_renamed_integration_test_selects_only_new_binary(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-nodes"
            tests = crate / "tests"
            tests.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-nodes"\n')
            (tests / "new_preset.rs").write_text("#[test] fn preset() {}\n")
            # effective_paths uses --no-renames: both old and new paths arrive.
            workspace = synthetic_workspace(d, [
                "crates/manifold-nodes/tests/old_preset.rs",
                "crates/manifold-nodes/tests/new_preset.rs",
            ])
            (Path(d) / "crates/manifold-nodes/tests/old_preset.rs").unlink()
            plan = cpu_scope.plan_for_paths([
                "crates/manifold-nodes/tests/old_preset.rs",
                "crates/manifold-nodes/tests/new_preset.rs",
            ], d, workspace=workspace)
            self.assertEqual(plan.filters, {
                "(package(=manifold-nodes) & binary(=new_preset))",
            })
            self.assertEqual(plan.packages, {"manifold-nodes"})

    def test_shared_test_code_selects_the_binaries_that_use_it(self):
        with tempfile.TemporaryDirectory() as d:
            crate = Path(d) / "crates/manifold-nodes"
            tests = crate / "tests"
            (tests / "support").mkdir(parents=True)
            (tests / "proofs").mkdir()
            (crate / "Cargo.toml").write_text('[package]\nname = "manifold-nodes"\n')
            (tests / "abi.rs").write_text("mod support {\n    pub mod cases;\n}\n")
            (tests / "layout.rs").write_text('#[path = "support/cases.rs"]\nmod cases;\n')
            (tests / "other.rs").write_text("")
            (tests / "proofs/main.rs").write_text("")
            (tests / "support/cases.rs").write_text("")
            (tests / "proofs/water.rs").write_text("")
            workspace = synthetic_workspace(d, ["crates/manifold-nodes/tests/support/cases.rs",
                                                "crates/manifold-nodes/tests/proofs/water.rs"])
            plan = cpu_scope.plan_for_paths(["crates/manifold-nodes/tests/support/cases.rs",
                                             "crates/manifold-nodes/tests/proofs/water.rs"], d,
                                            workspace=workspace)
            self.assertEqual(plan.filters, {
                "(package(=manifold-nodes) & binary(=abi))",
                "(package(=manifold-nodes) & binary(=layout))",
                "(package(=manifold-nodes) & binary(=proofs))",
            })

    def test_bundled_preset_json_selects_preset_contracts(self):
        with tempfile.TemporaryDirectory() as d:
            workspace = synthetic_workspace(d, ["crates/manifold-nodes/assets/generator-presets/Water.json"])
            plan = cpu_scope.plan_for_paths(["crates/manifold-nodes/assets/generator-presets/Water.json"], d,
                                            workspace=workspace)
            self.assertEqual(plan.filterset, "(package(=manifold-nodes) & test(/^bundled_presets::/))")
            self.assertEqual(plan.packages, {"manifold-nodes"})

    def test_primitive_source_selects_the_uniform_layout_proofs(self):
        with tempfile.TemporaryDirectory() as d:
            workspace = synthetic_workspace(d, ["crates/manifold-node-engine/src/water/primitives/blob_bounds.rs"])
            plan = cpu_scope.plan_for_paths(
                ["crates/manifold-node-engine/src/water/primitives/blob_bounds.rs"], d,
                workspace=workspace)
            self.assertIn("(package(=manifold-nodes) & test(/^uniform_layout_proof::/))", plan.filters)
            self.assertIn("(package(=manifold-nodes) & test(/^uniform_layout_extended::/))", plan.filters)
            self.assertNotIn("test(/^wgsl_validation::/)", plan.filterset)

    def test_shader_selects_wgsl_validation_and_hand_abi_proof(self):
        with tempfile.TemporaryDirectory() as d:
            workspace = synthetic_workspace(d, ["crates/manifold-node-engine/src/water/primitives/shaders/blob_bounds.wgsl"])
            plan = cpu_scope.plan_for_paths(
                ["crates/manifold-node-engine/src/water/primitives/shaders/blob_bounds.wgsl"], d,
                workspace=workspace)
            self.assertEqual(plan.filters, {
                "(package(=manifold-nodes) & test(/^uniform_layout_proof::/))",
                "(package(=manifold-nodes) & test(/^uniform_layout_extended::/))",
                "(package(=manifold-nodes) & test(/^wgsl_validation::/))",
                "(package(=manifold-nodes) & test(regenerates_in_sync))",
            })
            effect = cpu_scope.plan_for_paths(["crates/manifold-compositor/src/effects/shaders/presentation.wgsl"], d,
                                              workspace=workspace)
            self.assertEqual(effect.filterset, "(package(=manifold-nodes) & test(/^wgsl_validation::/))")

    def test_flow_scope_uses_only_effective_paths(self):
        import run_ui_flows
        manifest = {"path_triggers": {"crates/manifold-ui/": ["ui"]}}
        with patch.object(diff_scope, "git", return_value="base\n"), \
                patch.object(diff_scope, "effective_paths", return_value=([], ["crates/manifold-ui/src/lib.rs"])) as scope:
            self.assertEqual(run_ui_flows.filters_for_touched("origin/main...HEAD", manifest), ([], {}))
            scope.assert_called_once_with(run_ui_flows.ROOT, "base", "HEAD")


class CancellationTests(unittest.TestCase):
    def test_gate_imports_work_in_both_orders(self):
        scripts = str(Path(__file__).resolve().parent)
        for modules in ('landing_gate, gpu_proofs_gate', 'gpu_proofs_gate, landing_gate'):
            result = subprocess.run([sys.executable, '-B', '-c',
                f'import sys; sys.path.insert(0, {scripts!r}); import {modules}'],
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_proof_cleanup_timeout_retains_ownership_without_killing_runner(self):
        proc = Mock(pid=123, _requires_cleanup=True)
        proc.poll.return_value = None
        proc.wait.side_effect = [subprocess.TimeoutExpired('proof', 5), 143]
        with patch.object(landing_gate, 'kill_tree') as kill, \
                patch.object(landing_gate.os, 'killpg') as killpg:
            landing_gate.stop_child(proc, graceful=True)
        proc.send_signal.assert_called_once_with(signal.SIGTERM)
        self.assertEqual(proc.wait.call_count, 2)
        self.assertEqual(proc.wait.call_args.kwargs, {})
        kill.assert_not_called()
        killpg.assert_not_called()

    def test_cancel_proof_descendants_without_process_enumeration(self):
        scripts = str(Path(__file__).resolve().parent)
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                binary = (
                    'import os, pathlib, time\n'
                    f'pathlib.Path({str(root / "test.pid")!r}).write_text(str(os.getpid()))\n'
                    'print("fake proof running", flush=True)\n'
                    'time.sleep(120)\n')
                cargo = (
                    'import os, pathlib, subprocess, sys\n'
                    f'pathlib.Path({str(root / "cargo.pid")!r}).write_text(str(os.getpid()))\n'
                    f'subprocess.run([sys.executable, "-u", "-c", {binary!r}])\n')
                runner = root / 'gpu_proofs_gate.py'
                runner.write_text(
                    'import sys\nfrom pathlib import Path\n'
                    f'sys.path.insert(0, {scripts!r})\n'
                    'import gpu_proofs_gate as p\n'
                    f'p.cargo_test_cmd = lambda *a: [sys.executable, "-u", "-c", {cargo!r}]\n'
                    'p._main = lambda: p.run_gate(Path("/fake/Cargo.toml"), [], [])[0]\n'
                    'status = p.main()\n'
                    f'Path({str(root / "cleanup-ready")!r}).touch()\n'
                    'import time\n'
                    f'while not Path({str(root / "cleanup-release")!r}).exists(): time.sleep(.01)\n'
                    'sys.exit(status)\n')
                parent = (
                    'import pathlib, sys\n'
                    f'sys.path.insert(0, {scripts!r})\n'
                    'import landing_gate as g, gpu_queue\n'
                    f'root = pathlib.Path({directory!r})\n'
                    'original = g.subprocess.run\n'
                    'def no_ps(cmd, *a, **k):\n'
                    ' if cmd[0] == "ps": raise OSError("fake ps unavailable")\n'
                    ' return original(cmd, *a, **k)\n'
                    'g.subprocess.run = no_ps\n'
                    'def body(stack):\n'
                    ' stack.enter_context(gpu_queue.hold("fake proof", directory=root / "queue"))\n'
                    ' g.run_cmd([sys.executable, "-u", str(root / "gpu_proofs_gate.py")], root, 120, live_log=root / "proof.log")\n'
                    'g._main = body\n'
                    'sys.argv = ["landing_gate.py", "--repo", str(root)]\n'
                    'sys.exit(g.main())\n')
                proc = subprocess.Popen([sys.executable, '-u', '-c', parent],
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, start_new_session=True)
                try:
                    deadline = time.monotonic() + 10
                    while not (root / 'test.pid').exists() and time.monotonic() < deadline:
                        time.sleep(.01)
                    self.assertTrue((root / 'test.pid').exists())
                    cargo_pid = int((root / 'cargo.pid').read_text())
                    test_pid = int((root / 'test.pid').read_text())
                    self.assertTrue((root / 'queue/gpu.holder').exists())
                    proc.send_signal(sig)
                    deadline = time.monotonic() + 10
                    while not (root / 'cleanup-ready').exists() and time.monotonic() < deadline:
                        time.sleep(.01)
                    self.assertTrue((root / 'cleanup-ready').exists())
                    self.assertFalse(process_alive(cargo_pid))
                    self.assertFalse(process_alive(test_pid))
                    self.assertTrue((root / 'queue/gpu.holder').exists())
                    (root / 'cleanup-release').touch()
                    output, _ = proc.communicate(timeout=15)
                    self.assertEqual(proc.returncode, 128 + sig, output)
                    self.assertIn('descendants stopped', (root / 'proof.log').read_text())
                    self.assertFalse(process_alive(cargo_pid))
                    self.assertFalse(process_alive(test_pid))
                    self.assertFalse((root / 'queue/gpu.holder').exists())
                finally:
                    (root / 'cleanup-release').touch()
                    if proc.poll() is None:
                        landing_gate.stop_child(proc)
                    proc.stdout.close()

    def test_signals_reap_children_release_lock_and_keep_incomplete_log(self):
        # Real dummy processes, private file lock; no Cargo or GPU device.
        scripts = str(Path(__file__).resolve().parent)
        for module in ('landing_gate', 'land_branch'):
            for sig in (signal.SIGINT, signal.SIGTERM):
                with self.subTest(module=module, signal=sig), tempfile.TemporaryDirectory() as d:
                    root = Path(d)
                    child_code = "import time; print('child ready', flush=True); time.sleep(120)"
                    gate_code = (
                        "import sys, pathlib, os\n"
                        f"sys.path.insert(0, {scripts!r})\n"
                        "import landing_gate as g, gpu_queue\n"
                        f"root = pathlib.Path({d!r})\n"
                        "def body(stack):\n"
                        " stack.enter_context(gpu_queue.hold('fake test', directory=root / 'queue'))\n"
                        " original = g.subprocess.Popen\n"
                        " def spawn(*a, **k):\n"
                        "  p = original(*a, **k)\n"
                        "  if a[0][0] == sys.executable: (root / 'child.pid').write_text(str(p.pid))\n"
                        "  return p\n"
                        " g.subprocess.Popen = spawn\n"
                        f" g.run_cmd([sys.executable, '-u', '-c', {child_code!r}], root, 120, live_log=root / 'child.log')\n"
                        "g._main = body\n"
                        "sys.argv = ['landing_gate.py', '--repo', str(root)]\n"
                        "sys.exit(g.main())\n")
                    if module == 'landing_gate':
                        code = gate_code
                    else:
                        code = (
                            "import sys, pathlib\n"
                            f"sys.path.insert(0, {scripts!r})\n"
                            "import land_branch as land\n"
                            f"root = pathlib.Path({d!r})\n"
                            f"land._main = lambda: land.run_landing_gate([sys.executable, '-u', '-c', {gate_code!r}], root, root / 'gate.log')\n"
                            "sys.argv = ['land_branch.py', '--worktree', str(root)]\n"
                            "sys.exit(land.main())\n")
                    with (root / 'outer.log').open('w') as output:
                        proc = subprocess.Popen([sys.executable, '-u', '-c', code], stdout=output,
                                                stderr=subprocess.STDOUT, start_new_session=True)
                        try:
                            deadline = time.monotonic() + 10
                            while time.monotonic() < deadline and not (root / 'child.log').exists():
                                if proc.poll() is not None:
                                    self.fail((root / 'outer.log').read_text())
                                time.sleep(.02)
                            self.assertTrue((root / 'child.pid').exists())
                            child = int((root / 'child.pid').read_text())
                            proc.send_signal(sig)
                            self.assertEqual(proc.wait(timeout=15), 128 + sig)
                            self.assertFalse(process_alive(child))
                            self.assertFalse((root / 'queue/gpu.holder').exists())
                            import fcntl
                            with (root / 'queue/gpu.lock').open('a') as lock:
                                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            logs = list((root / 'target/landing-logs').glob('incomplete-*.log'))
                            self.assertTrue(logs)
                            self.assertIn('[INCOMPLETE]', logs[0].read_text())
                        finally:
                            if proc.poll() is None:
                                landing_gate.stop_child(proc)


class DeliveryTests(unittest.TestCase):
    def test_delivery_requests_complete_run_only_for_named_red(self):
        # Ordinary landings retain the cheap-first default. A named red asks
        # for complete coverage; missing reason/coverage still cannot land.
        cases = [[], ["--named-red", "BUG-test", "--reason", "reviewed"],
                 ["--named-red", "BUG-test"],
                 ["--named-red", "BUG-test", "--reason", "reviewed", "--skip-gpu", "deferred"]]
        for extra in cases:
            with self.subTest(extra=extra), tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                stack.enter_context(patch.object(sys, "argv", ["land_branch.py", "codex/test", "--worktree", d, "--message", "test", *extra]))
                stack.enter_context(patch.object(land_branch, "step"))
                gate = stack.enter_context(patch.object(land_branch, "run_landing_gate", side_effect=RuntimeError("stop before delivery")))
                with self.assertRaisesRegex(RuntimeError, "stop before delivery"):
                    land_branch.main()
                argv = gate.call_args.args[0]
                self.assertNotIn("--fail-fast", argv)
                self.assertEqual("--keep-going" in argv, "--named-red" in extra)
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

    def test_named_red_cannot_waive_boundary_stop(self):
        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            stack.enter_context(patch.object(sys, 'argv', ['land_branch.py', 'lane/test',
                '--worktree', d, '--message', 'test', '--named-red', 'BUG-test', '--reason', 'reviewed']))
            step = stack.enter_context(patch.object(land_branch, 'step'))
            stack.enter_context(patch.object(land_branch, 'run_landing_gate', return_value=1))
            with self.assertRaises(SystemExit):
                land_branch.main()
            self.assertFalse(any(c.args[0] in ('no-gate verdict', 'merge --no-ff to main', 'push main')
                                 for c in step.call_args_list))

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
