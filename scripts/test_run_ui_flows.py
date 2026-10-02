#!/usr/bin/env python3
"""Flow-gate runner: build once with no GPU lock, run every flow under one hold,
and flush each verdict as it happens. No Cargo, no GPU, no real flows."""
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest.mock import patch

import run_ui_flows

SCRIPTS = Path(__file__).resolve().parent
BINARY = "/fake/target/debug/manifold"


def artifact_line(executable):
    return json.dumps({"reason": "compiler-artifact", "target": {"name": "manifold"},
                       "executable": executable})


class RunnerTests(unittest.TestCase):
    def exercise(self, flows, argv=(), build_exit=0, failing=(), xfail=None):
        """Run main() over a temp manifest; returns (code, events, calls, output)."""
        events, calls = [], []

        @contextlib.contextmanager
        def recording_hold(label, **kwargs):
            events.append(f"hold-enter:{label}")
            try:
                yield
            finally:
                events.append("hold-exit")

        def run(cmd, **kwargs):
            calls.append((cmd, kwargs))
            if cmd[:2] == ["cargo", "build"]:
                events.append("build")
                out = "\n".join(["not json", artifact_line(BINARY)]) if build_exit == 0 else ""
                return subprocess.CompletedProcess(cmd, build_exit, out, None)
            name = Path(cmd[4]).stem
            events.append(f"flow:{name}")
            code = 1 if name in failing else 0
            return subprocess.CompletedProcess(cmd, code, "", "assertion detail\n")

        with tempfile.TemporaryDirectory() as d, contextlib.ExitStack() as stack:
            flow_dir = Path(d)
            manifest = {"flows": flows, "expected_fail": xfail or {}, "unresolved": {}}
            (flow_dir / "manifest.json").write_text(json.dumps(manifest))
            for name in [*flows, *(xfail or {})]:
                (flow_dir / f"{name}.json").write_text("[]")
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(run_ui_flows, "FLOW_DIR", str(flow_dir)))
            stack.enter_context(patch.object(run_ui_flows, "MANIFEST", str(flow_dir / "manifest.json")))
            stack.enter_context(patch.object(run_ui_flows.subprocess, "run", side_effect=run))
            stack.enter_context(patch.object(run_ui_flows.gpu_queue, "hold", side_effect=recording_hold))
            stack.enter_context(patch.object(sys, "argv", ["run_ui_flows.py", *argv]))
            code = run_ui_flows.main()
        return code, events, calls, output.getvalue()

    def test_builds_once_then_runs_every_flow_inside_one_hold(self):
        code, events, _, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "gltfscene"}, xfail={"flow-c": {"scene": "inspector", "bug": "BUG-x"}},
            failing={"flow-c"})
        self.assertEqual(code, 0)
        self.assertEqual(events, ["build", "hold-enter:run_ui_flows: 3 flows",
                                  "flow:flow-a", "flow:flow-b", "flow:flow-c", "hold-exit"])
        self.assertRegex(output, r"PASS   flow-a  \[timeline\]  \d+\.\ds")
        self.assertIn("XFAIL  flow-c", output)

    def test_flows_run_the_built_binary_directly(self):
        _, _, calls, _ = self.exercise({"flow-a": "timeline"})
        build, flow = calls
        self.assertEqual(build[0], run_ui_flows.BUILD_CMD)
        self.assertIn("ui-snapshot,perf-soak", build[0])
        self.assertEqual(flow[0], [BINARY, "ui-snap", "timeline", "--script",
                                   os.path.join("scripts", "ui-flows", "flow-a.json")])
        self.assertEqual(flow[1]["cwd"], run_ui_flows.ROOT)

    def test_failure_is_reported_with_the_harness_tail(self):
        code, _, _, output = self.exercise({"flow-a": "timeline"}, failing={"flow-a"})
        self.assertEqual(code, 1)
        self.assertIn("FAIL   flow-a  [timeline]", output)
        self.assertIn("exit=1  assertion detail", output)

    def test_build_failure_runs_no_flow_and_takes_no_lock(self):
        code, events, _, output = self.exercise({"flow-a": "timeline"}, build_exit=101)
        self.assertEqual(code, 2)
        self.assertEqual(events, ["build"])
        self.assertIn("build FAILED (exit 101", output)

    def test_empty_selection_builds_nothing_and_takes_no_lock(self):
        code, events, calls, _ = self.exercise({"flow-a": "timeline"}, argv=["no-such-flow"])
        self.assertEqual(code, 0)
        self.assertEqual(events, [])
        self.assertEqual(calls, [])

    def test_verdicts_reach_a_pipe_before_the_next_flow_starts(self):
        # Real process, real pipe: a caller that times out must already hold
        # every finished flow's line. The second flow blocks until the test
        # has read the first flow's PASS from the pipe.
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            flow_dir = d / "flows"
            flow_dir.mkdir()
            (flow_dir / "manifest.json").write_text(json.dumps(
                {"flows": {"flow-a": "timeline", "flow-b": "timeline"}}))
            for name in ("flow-a", "flow-b"):
                (flow_dir / f"{name}.json").write_text("[]")
            release = d / "release"
            binary = d / "manifold"
            binary.write_text(textwrap.dedent(f"""\
                #!/bin/sh
                case "$4" in *flow-b*)
                  while [ ! -e '{release}' ]; do sleep 0.05; done ;;
                esac
                exit 0
                """))
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
            build = [sys.executable, "-c", f"print({artifact_line(str(binary))!r})"]
            child = textwrap.dedent(f"""\
                import sys
                sys.path.insert(0, {str(SCRIPTS)!r})
                import run_ui_flows
                run_ui_flows.FLOW_DIR = {str(flow_dir)!r}
                run_ui_flows.MANIFEST = {str(flow_dir / "manifest.json")!r}
                run_ui_flows.BUILD_CMD = {build!r}
                sys.argv = ["run_ui_flows.py"]
                sys.exit(run_ui_flows.main())
                """)
            env = dict(os.environ, MANIFOLD_GPU_QUEUE_DIR=str(d / "queue"))
            proc = subprocess.Popen([sys.executable, "-c", child], stdout=subprocess.PIPE,
                                    text=True, env=env)
            try:
                seen = []
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    line = proc.stdout.readline()
                    if not line:
                        break
                    seen.append(line)
                    if "flow-a" in line:
                        break
                self.assertTrue(any(l.startswith("  PASS   flow-a") for l in seen), seen)
                self.assertIsNone(proc.poll(), "runner finished before flow-b was released")
                release.touch()
                rest = proc.stdout.read()
                self.assertEqual(proc.wait(30), 0)
                self.assertIn("PASS   flow-b", rest)
            finally:
                release.touch()
                if proc.poll() is None:
                    proc.kill()
                proc.stdout.close()


if __name__ == "__main__":
    unittest.main()
