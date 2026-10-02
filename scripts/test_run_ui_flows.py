#!/usr/bin/env python3
"""Flow-gate runner: build once with no GPU lock, run the flows through one
`ui-snap batch` process under one hold, fall back to solo runs for anything the
batch can't vouch for, and flush each verdict as it happens. No Cargo, no GPU,
no real flows: a fake binary speaks the batch protocol."""
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

# Behaviour per flow stem, read from $FAKE_FLOWS: pass (default), fail, rerun,
# panic (rerun record then exit 101), die (no record, killed), wait (block on
# $FAKE_RELEASE), batchfail (fails batched, passes solo). Every invocation is
# appended to $FAKE_LOG. With $FAKE_OUT set, each finished flow writes
# <FAKE_OUT>/<scene>/run-<stem>/01.png whose bytes "_art_<stem>" picks: same
# (default), batchdiff (differs by mode), unstable (differs every run).
FAKE_BINARY = textwrap.dedent("""\
    #!{python}
    import json, os, signal, sys, time
    from pathlib import Path
    behaviour = json.loads(Path(os.environ["FAKE_FLOWS"]).read_text())
    def log(line):
        with open(os.environ["FAKE_LOG"], "a") as f:
            f.write(line + "\\n")
    def act(stem):
        b = behaviour.get(stem, "pass")
        if b == "wait":
            while not os.path.exists(os.environ["FAKE_RELEASE"]):
                time.sleep(0.05)
        return b
    def write(scene, stem, mode):
        if not os.environ.get("FAKE_OUT"):
            return
        d = Path(os.environ["FAKE_OUT"], scene, "run-" + stem)
        d.mkdir(parents=True, exist_ok=True)
        art = behaviour.get("_art_" + stem, "same")
        body = {{"same": "pixels", "batchdiff": "pixels-" + mode,
                 "unstable": str(time.time_ns())}}[art]
        (d / "01.png").write_text(body)
    args = sys.argv[1:]
    assert args[0] == "ui-snap", args
    if args[1] == "batch":
        pairs = list(zip(args[2::2], args[3::2]))
        stems = [Path(s).stem for _, s in pairs]
        log("batch " + " ".join(stems) + " cwd=" + os.getcwd())
        if behaviour.get("_batch") == "empty":
            print("batch: refused", file=sys.stderr)
            sys.exit(2)
        for i, ((scene, _), stem) in enumerate(zip(pairs, stems)):
            print("chatter from the flow")
            print("@@ui-snap-batch@@ " + json.dumps({{"begin": i}}), flush=True)
            b = act(stem)
            if b == "die":
                print("thread 'main' hit a wall", file=sys.stderr, flush=True)
                os.kill(os.getpid(), signal.SIGKILL)
            if b == "panic":
                print("@@ui-snap-batch@@ " + json.dumps({{"index": i, "rerun": "panicked"}}), flush=True)
                sys.exit(101)
            if b == "rerun":
                rec = {{"index": i, "rerun": "registry moved"}}
            elif b in ("fail", "batchfail"):
                write(scene, stem, "batch")
                rec = {{"index": i, "code": 1, "tail": "FAILED " + stem, "seconds": 0.25}}
            else:
                write(scene, stem, "batch")
                rec = {{"index": i, "code": 0, "tail": "", "seconds": 0.25}}
            print("@@ui-snap-batch@@ " + json.dumps(rec), flush=True)
        sys.exit(0)
    scene, script = args[1], args[3]
    assert args[2] == "--script", args
    stem = Path(script).stem
    log("solo " + stem + " " + scene)
    b = act(stem)
    write(scene, stem, "solo")
    if b in ("fail", "die", "panic") and behaviour.get("_solo_" + stem) != "pass":
        print("solo detail", file=sys.stderr)
        print("FAILED " + stem, file=sys.stderr)
        sys.exit(1)
    sys.exit(0)
    """)


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.log = self.tmp / "log"
        self.binary = self.tmp / "manifold"
        self.binary.write_text(FAKE_BINARY.format(python=sys.executable))
        self.binary.chmod(self.binary.stat().st_mode | stat.S_IXUSR)
        self.flow_dir = self.tmp / "flows"
        self.flow_dir.mkdir()

    def tearDown(self):
        subprocess.run(["rm", "-rf", str(self.tmp)], check=False)

    def write_manifest(self, flows, xfail=None):
        manifest = {"flows": flows, "expected_fail": xfail or {}, "unresolved": {}}
        (self.flow_dir / "manifest.json").write_text(json.dumps(manifest))
        for name in [*flows, *(xfail or {})]:
            (self.flow_dir / f"{name}.json").write_text("[]")

    def build_cmd(self, ok=True):
        artifact = json.dumps({"reason": "compiler-artifact", "target": {"name": "manifold"},
                               "executable": str(self.binary)})
        body = (f"open({str(self.log)!r}, 'a').write('build\\n'); "
                + (f"print('not json'); print({artifact!r})" if ok else "raise SystemExit(101)"))
        return [sys.executable, "-c", body]

    def events(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def exercise(self, flows, xfail=None, behaviour=None, argv=(), build_ok=True):
        """Run main() over a temp manifest; returns (exit code, events, output)."""
        self.write_manifest(flows, xfail)
        (self.tmp / "behaviour.json").write_text(json.dumps(behaviour or {}))

        @contextlib.contextmanager
        def recording_hold(label, **kwargs):
            with open(self.log, "a") as f:
                f.write(f"hold-enter:{label}\n")
            try:
                yield
            finally:
                with open(self.log, "a") as f:
                    f.write("hold-exit\n")

        with contextlib.ExitStack() as stack:
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(run_ui_flows, "FLOW_DIR", str(self.flow_dir)))
            stack.enter_context(patch.object(run_ui_flows, "MANIFEST", str(self.flow_dir / "manifest.json")))
            stack.enter_context(patch.object(run_ui_flows, "BUILD_CMD", self.build_cmd(build_ok)))
            stack.enter_context(patch.object(run_ui_flows.gpu_queue, "hold", side_effect=recording_hold))
            stack.enter_context(patch.object(sys, "argv", ["run_ui_flows.py", *argv]))
            stack.enter_context(patch.dict(os.environ, {
                "FAKE_FLOWS": str(self.tmp / "behaviour.json"), "FAKE_LOG": str(self.log),
                "FAKE_RELEASE": str(self.tmp / "release")}))
            code = run_ui_flows.main()
        return code, self.events(), output.getvalue()

    def test_builds_once_then_batches_every_flow_inside_one_hold(self):
        code, events, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "gltfscene"},
            xfail={"flow-c": {"scene": "inspector", "bug": "BUG-x"}},
            behaviour={"flow-c": "fail"})
        self.assertEqual(code, 0)
        cwd = f" cwd={run_ui_flows.ROOT}"
        self.assertEqual(events, ["build", "hold-enter:run_ui_flows: 3 flows",
                                  "batch flow-a flow-b" + cwd, "batch flow-c" + cwd, "hold-exit"])
        self.assertIn("PASS   flow-a  [timeline]  0.2s", output)
        self.assertIn("PASS   flow-b  [gltfscene]  0.2s", output)
        self.assertIn("XFAIL  flow-c  [inspector]", output)
        self.assertNotIn("chatter", output)

    def test_batch_gets_scene_script_pairs(self):
        calls = []
        real_popen = subprocess.Popen

        def popen(cmd, **kwargs):
            if "ui-snap" in cmd:  # subprocess.run's build call comes through here too
                calls.append((cmd, kwargs))
            return real_popen(cmd, **kwargs)

        with patch.object(run_ui_flows.subprocess, "Popen", side_effect=popen):
            self.exercise({"flow-a": "timeline", "flow-b": "gltfscene"})
        (cmd, kwargs), = calls
        script = lambda n: os.path.join("scripts", "ui-flows", f"{n}.json")
        self.assertEqual(cmd, [str(self.binary), "ui-snap", "batch",
                               "timeline", script("flow-a"), "gltfscene", script("flow-b")])
        self.assertEqual(kwargs["cwd"], run_ui_flows.ROOT)

    def test_build_is_the_xtask_binary(self):
        self.assertEqual(run_ui_flows.BUILD_CMD[:5], ["cargo", "build", "--quiet", "-p", "manifold-app"])
        self.assertIn("ui-snapshot,perf-soak", run_ui_flows.BUILD_CMD)

    def test_batched_failure_is_reported_with_the_solo_tail(self):
        code, _, output = self.exercise({"flow-a": "timeline"}, behaviour={"flow-a": "fail"})
        self.assertEqual(code, 1)
        self.assertIn("FAIL   flow-a  [timeline]  0.2s  exit=1  FAILED flow-a", output)

    def test_rerun_record_runs_that_flow_solo_and_the_batch_carries_on(self):
        code, events, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "inspector", "flow-c": "timeline"},
            behaviour={"flow-b": "rerun"})
        self.assertEqual(code, 0)
        self.assertEqual([e.split(" cwd=")[0] for e in events[2:-1]],
                         ["batch flow-a flow-b flow-c", "solo flow-b inspector"])
        self.assertIn("batch handed back flow-b: registry moved", output)
        self.assertIn("PASS   flow-b  [inspector]", output)

    def test_flow_that_kills_the_batch_runs_solo_and_the_rest_rebatch(self):
        code, events, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "inspector", "flow-c": "timeline"},
            behaviour={"flow-b": "die"})
        self.assertEqual(code, 1)
        self.assertEqual([e.split(" cwd=")[0] for e in events[2:-1]],
                         ["batch flow-a flow-b flow-c", "solo flow-b inspector", "batch flow-c"])
        self.assertIn("batch died in flow-b, exit -9: thread 'main' hit a wall", output)
        self.assertIn("FAIL   flow-b  [inspector]", output)
        self.assertIn("exit=1  FAILED flow-b", output)
        self.assertIn("PASS   flow-c", output)

    def test_panicked_flow_reruns_solo_with_the_solo_verdict(self):
        code, events, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "inspector", "flow-c": "timeline"},
            behaviour={"flow-a": "panic", "_solo_flow-a": "pass"})
        self.assertEqual(code, 0)
        self.assertEqual([e.split(" cwd=")[0] for e in events[2:-1]],
                         ["batch flow-a flow-b flow-c", "solo flow-a timeline",
                          "batch flow-b flow-c"])
        self.assertIn("batch handed back flow-a: panicked", output)
        for name in ("flow-a", "flow-b", "flow-c"):
            self.assertIn(f"PASS   {name}", output)

    def test_batch_that_runs_nothing_falls_back_to_solo_for_all(self):
        code, events, output = self.exercise(
            {"flow-a": "timeline", "flow-b": "inspector"}, behaviour={"_batch": "empty"})
        self.assertEqual(code, 0)
        self.assertEqual([e.split(" cwd=")[0] for e in events[2:-1]],
                         ["batch flow-a flow-b", "solo flow-a timeline", "solo flow-b inspector"])
        self.assertIn("batch exited 2 without running a flow", output)

    def test_build_failure_runs_no_flow_and_takes_no_lock(self):
        code, events, output = self.exercise({"flow-a": "timeline"}, build_ok=False)
        self.assertEqual(code, 2)
        self.assertEqual(events, ["build"])
        self.assertIn("build FAILED (exit 101", output)

    def test_empty_selection_builds_nothing_and_takes_no_lock(self):
        code, events, _ = self.exercise({"flow-a": "timeline"}, argv=["no-such-flow"])
        self.assertEqual(code, 0)
        self.assertEqual(events, [])

    def test_verdicts_reach_a_pipe_before_the_next_flow_starts(self):
        # Real process, real pipe: a caller that times out must already hold
        # every finished flow's line. flow-b blocks inside the batch until the
        # test has read flow-a's PASS from the pipe.
        self.write_manifest({"flow-a": "timeline", "flow-b": "timeline"})
        (self.tmp / "behaviour.json").write_text(json.dumps({"flow-b": "wait"}))
        release = self.tmp / "release"
        child = textwrap.dedent(f"""\
            import sys
            sys.path.insert(0, {str(SCRIPTS)!r})
            import run_ui_flows
            run_ui_flows.FLOW_DIR = {str(self.flow_dir)!r}
            run_ui_flows.MANIFEST = {str(self.flow_dir / "manifest.json")!r}
            run_ui_flows.BUILD_CMD = {self.build_cmd()!r}
            sys.argv = ["run_ui_flows.py"]
            sys.exit(run_ui_flows.main())
            """)
        env = dict(os.environ, MANIFOLD_GPU_QUEUE_DIR=str(self.tmp / "queue"),
                   FAKE_FLOWS=str(self.tmp / "behaviour.json"), FAKE_LOG=str(self.log),
                   FAKE_RELEASE=str(release))
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
