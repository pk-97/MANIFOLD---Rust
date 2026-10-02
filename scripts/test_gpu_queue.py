#!/usr/bin/env python3
"""Tests for scripts/gpu_queue.py: serialization, crash release, re-entrancy.

Every test uses a private MANIFOLD_GPU_QUEUE_DIR, so none of them touch (or
wait on) the real machine-wide lock.
"""

import contextlib
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch

import gpu_queue

QUEUE = str(Path(__file__).resolve().parent / "gpu_queue.py")


class GpuQueueTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        self.env = {**os.environ, "MANIFOLD_GPU_QUEUE_DIR": str(self.dir / "q")}
        directory = patch.object(gpu_queue, "queue_dir", return_value=self.dir / "q")
        directory.start()
        self.addCleanup(directory.stop)
        self.procs = []

    def tearDown(self):
        for proc in self.procs:
            with_group_kill(proc)
        self._tmp.cleanup()

    def spawn(self, *args, **kwargs):
        proc = subprocess.Popen(
            [QUEUE, *args], env=self.env, start_new_session=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kwargs)
        self.procs.append(proc)
        return proc

    def wait_for(self, predicate, timeout=10.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.02)
        self.fail("timed out waiting for condition")

    def test_two_concurrent_holders_serialize(self):
        log = self.dir / "events.log"
        job = f"echo start >> {log}; sleep 0.7; echo end >> {log}"
        first = self.spawn("--", "sh", "-c", job)
        self.wait_for(lambda: log.exists())
        second = self.spawn("--", "sh", "-c", job)
        _, second_err = second.communicate(timeout=30)
        first.communicate(timeout=30)
        self.assertEqual(log.read_text().split(), ["start", "end", "start", "end"])
        self.assertIn("waiting for the GPU: held by pid", second_err)
        self.assertIn("holding for", second_err)
        self.assertIn("acquired after", second_err)

    def test_exit_code_is_the_commands(self):
        proc = self.spawn("--", "sh", "-c", "exit 7")
        proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 7)

    def test_killed_holder_releases(self):
        holder = self.spawn("--", "sleep", "60")
        self.wait_for(lambda: (self.dir / "q" / "gpu.holder").exists())
        busy = subprocess.run([QUEUE, "status"], env=self.env, capture_output=True, text=True)
        self.assertEqual(busy.returncode, 1)
        self.assertIn("GPU busy", busy.stdout)

        holder.kill()  # SIGKILL: no cleanup code runs
        holder.wait(timeout=10)
        free = subprocess.run([QUEUE, "status"], env=self.env, capture_output=True, text=True)
        self.assertEqual(free.returncode, 0, free.stdout)

        # A fresh run gets in immediately even though the stale record remains.
        started = time.monotonic()
        nxt = subprocess.run([QUEUE, "--", "true"], env=self.env, capture_output=True,
                             text=True, timeout=30)
        self.assertEqual(nxt.returncode, 0)
        self.assertLess(time.monotonic() - started, 5)
        self.assertNotIn("waiting", nxt.stderr)

    def test_nested_call_does_not_deadlock(self):
        # gate script (holds) -> cargo-like child (takes the lock again).
        proc = self.spawn("--", QUEUE, "--", "sh", "-c", "echo inner-ran")
        out, err = proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 0, err)
        self.assertIn("inner-ran", out)
        self.assertNotIn("waiting", err)

    def test_nested_through_an_intermediate_process(self):
        # holder -> sh -> gpu_queue: the holder is a grandparent, not a parent.
        proc = self.spawn("--", "sh", "-c", f"'{QUEUE}' -- echo deep")
        out, err = proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 0, err)
        self.assertIn("deep", out)
        self.assertNotIn("waiting", err)

    def test_unrelated_process_still_waits_for_a_holder(self):
        holder = self.spawn("--", "sleep", "60")
        self.wait_for(lambda: (self.dir / "q" / "gpu.holder").exists())
        waiter = self.spawn("--", "true")
        time.sleep(1.0)
        self.assertIsNone(waiter.poll(), "unrelated process must wait, not slip in")
        holder.send_signal(signal.SIGTERM)  # forwarded to sleep; lock released
        holder.communicate(timeout=10)
        waiter.communicate(timeout=30)
        self.assertEqual(waiter.returncode, 0)

    def test_same_process_hold_is_reentrant(self):
        os.environ["MANIFOLD_GPU_QUEUE_DIR"] = str(self.dir / "q")
        try:
            with gpu_queue.hold("outer"):
                with gpu_queue.hold("inner"):
                    pass
                busy = subprocess.run([QUEUE, "status"], env=self.env,
                                      capture_output=True, text=True)
                self.assertEqual(busy.returncode, 1, "outer hold must outlive inner")
            free = subprocess.run([QUEUE, "status"], env=self.env,
                                  capture_output=True, text=True)
            self.assertEqual(free.returncode, 0)
        finally:
            del os.environ["MANIFOLD_GPU_QUEUE_DIR"]

    def test_cargo_test_builds_before_the_lock_and_preserves_runtime_args(self):
        events = []

        @contextmanager
        def recording_hold(label, **kwargs):
            events.append(("hold-enter", label))
            try:
                yield
            finally:
                events.append(("hold-exit", label))

        child = type("Child", (), {"wait": lambda self: 0, "send_signal": lambda self, sig: None})()
        command = ["cargo", "+nightly", "test", "--release", "-p", "demo", "--", "--nocapture"]
        with patch.object(gpu_queue.subprocess, "run", side_effect=lambda cmd: events.append(("build", cmd)) or subprocess.CompletedProcess(cmd, 0)), \
             patch.object(gpu_queue.subprocess, "Popen", side_effect=lambda cmd: events.append(("run", cmd)) or child), \
             patch.object(gpu_queue, "hold", side_effect=recording_hold):
            self.assertEqual(gpu_queue.run_queued(command), 0)

        self.assertEqual(events, [
            ("build", ["cargo", "+nightly", "test", "--release", "-p", "demo", "--no-run", "--", "--nocapture"]),
            ("hold-enter", "cargo +nightly test --release -p demo -- --nocapture"),
            ("run", command),
            ("hold-exit", "cargo +nightly test --release -p demo -- --nocapture"),
        ])

    def test_cargo_run_build_excludes_runtime_args(self):
        events = []
        child = type("Child", (), {"wait": lambda self: 0, "send_signal": lambda self, sig: None})()
        command = ["cargo", "run", "--release", "-p", "demo", "--bin", "demo", "--", "--selftest"]
        with patch.object(gpu_queue.subprocess, "run", side_effect=lambda cmd: events.append(cmd) or subprocess.CompletedProcess(cmd, 0)), \
             patch.object(gpu_queue.subprocess, "Popen", return_value=child), \
             patch.object(gpu_queue, "hold", return_value=contextlib.nullcontext()):
            self.assertEqual(gpu_queue.run_queued(command), 0)
        self.assertEqual(events, [["cargo", "build", "--release", "-p", "demo", "--bin", "demo"]])

    def test_cargo_build_failure_takes_no_lock(self):
        command = ["cargo", "build", "-p", "demo"]
        with patch.object(gpu_queue.subprocess, "run", return_value=subprocess.CompletedProcess(command, 101)), \
             patch.object(gpu_queue, "hold", side_effect=AssertionError("build must not take the lock")):
            self.assertEqual(gpu_queue.run_queued(command), 101)

    def test_successful_cargo_build_and_no_run_take_no_lock(self):
        build = ["cargo", "build", "-p", "demo"]
        no_run = ["cargo", "test", "-p", "demo", "--no-run"]
        with patch.object(gpu_queue.subprocess, "run", side_effect=lambda cmd: subprocess.CompletedProcess(cmd, 0)), \
             patch.object(gpu_queue, "hold", side_effect=AssertionError("build-only command must not lock")):
            self.assertEqual(gpu_queue.run_queued(build), 0)
            self.assertEqual(gpu_queue.run_queued(no_run), 0)

    def test_cargo_prebuild_is_rejected_inside_an_inherited_hold(self):
        command = ["cargo", "run", "-p", "demo"]
        gpu_queue._held_depth = 1
        try:
            with patch.object(gpu_queue.subprocess, "run", side_effect=AssertionError("must not build")), \
                 patch.object(gpu_queue, "hold", side_effect=AssertionError("must not nest")):
                self.assertEqual(gpu_queue.run_queued(command), 2)
        finally:
            gpu_queue._held_depth = 0

    def test_unsupported_cargo_invocation_is_rejected_before_lock(self):
        command = ["cargo", "metadata", "--no-deps"]
        with patch.object(gpu_queue.subprocess, "run", side_effect=AssertionError("must reject")), \
             patch.object(gpu_queue, "hold", side_effect=AssertionError("must reject")):
            self.assertEqual(gpu_queue.run_queued(command), 2)

    def test_cargo_ancestor_hold_or_unknown_ancestry_refuses_build(self):
        for ancestors in ([42], []):
            with self.subTest(ancestors=ancestors), \
                    patch.object(gpu_queue, "read_holder", return_value={"pid": "42"}), \
                    patch.object(gpu_queue, "ancestor_pids", return_value=ancestors), \
                    patch.object(gpu_queue, "_run_build") as build:
                self.assertEqual(gpu_queue.run_queued(["cargo", "test"]), 2)
                build.assert_not_called()


def with_group_kill(proc):
    """Kill the whole session a test spawned, so no `sleep 60` outlives the test."""
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        proc.communicate(timeout=5)
    except (subprocess.TimeoutExpired, ValueError):
        pass


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *sys.argv[1:]])
