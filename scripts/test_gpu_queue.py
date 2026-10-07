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
import threading
import time
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import MagicMock, patch

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

    def test_landing_pending_record_is_removed_on_context_exit(self):
        pending_dir = self.dir / "q" / gpu_queue.LANDING_PENDING_DIR
        with gpu_queue.landing_pending(label="test landing"):
            records = gpu_queue.pending_landings(self.dir / "q")
            self.assertEqual(len(records), 1)
            self.assertEqual(records[0]["pid"], str(os.getpid()))
            self.assertEqual(records[0]["label"], "test landing")
            self.assertTrue(list(pending_dir.iterdir()))
        self.assertEqual(gpu_queue.pending_landings(self.dir / "q"), [])
        self.assertEqual(list(pending_dir.iterdir()), [])

    def test_stale_landing_record_is_removed_and_does_not_starve_nightly(self):
        pending_dir = self.dir / "q" / gpu_queue.LANDING_PENDING_DIR
        pending_dir.mkdir(parents=True)
        stale = pending_dir / "999999999.dead.pending"
        stale.write_text("pid=999999999\nnonce=dead\nlabel=stale\n")
        started = time.monotonic()
        held = gpu_queue.acquire("nightly", directory=self.dir / "q", priority="nightly",
                                 poll=0.01, report=0.1)
        try:
            self.assertLess(time.monotonic() - started, 2)
            self.assertFalse(stale.exists())
        finally:
            held.release()

    def test_pending_scan_leaves_atomic_write_and_permission_limited_owner_alone(self):
        pending_dir = self.dir / 'q' / gpu_queue.LANDING_PENDING_DIR
        pending_dir.mkdir(parents=True)
        temporary = pending_dir / '.unpublished.tmp'
        temporary.write_text('pid=')
        with gpu_queue.landing_pending(directory=self.dir / 'q'):
            with patch.object(gpu_queue.os, 'kill', side_effect=PermissionError):
                self.assertEqual(len(gpu_queue.pending_landings(self.dir / 'q')), 1)
            self.assertTrue(temporary.exists())

    def test_pid_reuse_removes_old_landing_identity(self):
        pending_dir = self.dir / "q" / gpu_queue.LANDING_PENDING_DIR
        pending_dir.mkdir(parents=True)
        marker = pending_dir / "1234.reused.pending"
        marker.write_text("pid=1234\nstart=proc:old\nnonce=dead\nlabel=old\n")
        with patch.object(gpu_queue.os, "kill"), \
                patch.object(gpu_queue, "process_start_identity", return_value="proc:new"):
            self.assertEqual(gpu_queue.pending_landings(self.dir / "q"), [])
        self.assertFalse(marker.exists(), "PID reuse must not keep a stale landing live")

    def test_nightly_freezes_batch_and_admits_before_new_landings(self):
        pending_dir = self.dir / "q" / gpu_queue.LANDING_PENDING_DIR
        pending_dir.mkdir(parents=True)
        start = gpu_queue.process_start_identity()
        old = pending_dir / f"{os.getpid()}.old.pending"
        body = f"pid={os.getpid()}\nstart={start}\nnonce={{}}\nlabel={{}}\n"
        old.write_text(body.format("old", "old"))
        acquired = threading.Event()
        result = []

        def wait_for_gpu():
            held = gpu_queue.acquire("nightly", directory=self.dir / "q", priority="nightly",
                                     poll=0.01, report=0.1)
            result.append(held)
            acquired.set()

        worker = threading.Thread(target=wait_for_gpu)
        worker.start()
        self.wait_for(lambda: (self.dir / "q" / gpu_queue.NIGHTLY_WAITING).exists())
        newer = []
        for index in range(8):
            marker = pending_dir / f"{os.getpid()}.new-{index}.pending"
            marker.write_text(body.format(f"new-{index}", f"new-{index}"))
            newer.append(marker)
        old.unlink()
        self.assertTrue(acquired.wait(2), "new arrivals must not starve the frozen nightly turn")
        worker.join(timeout=2)
        result[0].release()
        for marker in newer:
            marker.unlink()

    def test_inherited_holder_bypasses_nightly_fairness_wait(self):
        with patch.object(gpu_queue.fcntl, "flock", side_effect=BlockingIOError), \
                patch.object(gpu_queue, "_token_matches", return_value=True):
            held = gpu_queue.acquire("child", directory=self.dir / "q", priority="nightly",
                                     poll=0.01)
        self.assertIsNone(held.fd)
        held.release()

    def test_newer_normal_work_is_deferred_by_nightly_turn(self):
        pending_dir = self.dir / "q" / gpu_queue.LANDING_PENDING_DIR
        pending_dir.mkdir(parents=True)
        waiting = self.dir / "q" / gpu_queue.NIGHTLY_WAITING
        waiting.write_text("pid=9999\nstart=other\nbatch=old.pending\n")
        current = pending_dir / f"{os.getpid()}.new.pending"
        current.write_text(f"pid={os.getpid()}\nstart=self\nnonce=new\nlabel=new\n")

        def identity(pid=None):
            return "self" if pid in (None, os.getpid()) else "other"

        acquired = threading.Event()
        result = []

        def wait_for_gpu():
            result.append(gpu_queue.acquire("new landing", directory=self.dir / "q",
                                            poll=0.01, report=0.1))
            acquired.set()

        with patch.object(gpu_queue, "process_start_identity", side_effect=identity), \
                patch.object(gpu_queue.os, "kill"):
            worker = threading.Thread(target=wait_for_gpu)
            worker.start()
            self.assertFalse(acquired.wait(0.2))
            waiting.unlink()
            self.assertTrue(acquired.wait(2))
            worker.join(timeout=2)
        result[0].release()
        current.unlink()

    def test_admission_guard_keeps_one_nightly_turn_owner(self):
        entered = threading.Event()
        release = threading.Event()
        observed = []

        def first():
            with gpu_queue._admission_guard(self.dir / "q"):
                gpu_queue._write_nightly_waiting(self.dir / "q", {"old"})
                entered.set()
                release.wait(2)

        def second():
            entered.wait(2)
            with gpu_queue._admission_guard(self.dir / "q"):
                observed.append(gpu_queue._nightly_waiting(self.dir / "q"))

        one = threading.Thread(target=first)
        two = threading.Thread(target=second)
        one.start()
        two.start()
        self.assertTrue(entered.wait(2))
        time.sleep(0.05)
        self.assertFalse(observed)
        release.set()
        one.join(timeout=2)
        two.join(timeout=2)
        self.assertEqual(len(observed), 1)
        self.assertEqual(observed[0]["pid"], str(os.getpid()))
        gpu_queue._remove_own_nightly_waiting(self.dir / "q")

    def test_raw_heavy_run_cannot_create_reusable_receipt(self):
        passed = MagicMock()
        passed.reused.return_value = False
        child = type("Child", (), {"wait": lambda self: 0,
                                    "send_signal": lambda self, sig: None})()
        with patch("gate_passes.queued_proof", return_value=passed), \
                patch.object(gpu_queue, "_run_build", return_value=0), \
                patch.object(gpu_queue, "_ancestor_holds", return_value=False), \
                patch.object(gpu_queue, "hold", side_effect=lambda *a, **k: contextlib.nullcontext()), \
                patch.object(gpu_queue.subprocess, "Popen", return_value=child), \
                patch.object(gpu_queue.time, "monotonic", side_effect=[0, 90]):
            self.assertEqual(gpu_queue.run_queued(["cargo", "test"]), 0)
        passed.save.assert_not_called()

    def test_changed_inputs_while_queued_never_start_gpu_process(self):
        passed = MagicMock()
        passed.reused.return_value = False
        with patch('gate_passes.queued_proof', return_value=passed), \
                patch('gate_passes.changed_passes', side_effect=[[], ['gpu-proofs']]), \
                patch.object(gpu_queue, '_run_build', return_value=0), \
                patch.object(gpu_queue, '_ancestor_holds', return_value=False), \
                patch.object(gpu_queue, 'hold', side_effect=lambda *a, **k: contextlib.nullcontext()), \
                patch.object(gpu_queue.subprocess, 'Popen') as process:
            self.assertEqual(gpu_queue.run_queued(['cargo', 'test']), 2)
        process.assert_not_called()
        passed.save.assert_not_called()

    def test_nightly_waits_for_live_landing_then_acquires(self):
        acquired = threading.Event()
        result = []

        def wait_for_gpu():
            held = gpu_queue.acquire("nightly", directory=self.dir / "q", priority="nightly",
                                     poll=0.01, report=0.1)
            result.append(held)
            acquired.set()

        with gpu_queue.landing_pending(directory=self.dir / "q"):
            worker = threading.Thread(target=wait_for_gpu)
            worker.start()
            self.assertFalse(acquired.wait(0.2))
        self.assertTrue(acquired.wait(2))
        worker.join(timeout=2)
        result[0].release()

    def test_nightly_rechecks_pending_after_flock(self):
        calls = []
        pending = {"pid": str(os.getpid()), "label": "landing"}

        def pending_check(_directory):
            calls.append(len(calls))
            return [pending] if len(calls) == 2 else []

        with patch.object(gpu_queue, "pending_landings", side_effect=pending_check):
            held = gpu_queue.acquire("nightly", directory=self.dir / "q", priority="nightly",
                                     poll=0.01, report=0.1)
        try:
            self.assertGreaterEqual(len(calls), 2)
        finally:
            held.release()

    def test_token_admission_when_ancestor_walk_fails(self):
        with patch.dict(os.environ, {gpu_queue.HOLDER_ENV: "previous"}):
            holder = gpu_queue.acquire("outer")
            token = os.environ[gpu_queue.HOLDER_ENV]
            info = gpu_queue.read_holder(self.dir / "q")
            self.assertEqual(token, f"{os.getpid()}:{info['nonce']}")
            try:
                for candidate, allowed in ((token, True), (token + "stale", False), (None, False)):
                    with self.subTest(token=candidate), \
                            patch.dict(os.environ), \
                            patch.object(gpu_queue, "ancestor_pids", return_value=[]), \
                            patch.object(gpu_queue, "holder_command", return_value=""), \
                            patch.object(gpu_queue.time, "sleep", side_effect=RuntimeError("waited")):
                        if candidate is None:
                            os.environ.pop(gpu_queue.HOLDER_ENV, None)
                        else:
                            os.environ[gpu_queue.HOLDER_ENV] = candidate
                        if allowed:
                            child = gpu_queue.acquire("child")
                            self.assertIsNone(child.fd)
                            child.release()
                            self.assertEqual(gpu_queue.read_holder(self.dir / "q"), info)
                        else:
                            with self.assertRaisesRegex(RuntimeError, "waited"):
                                gpu_queue.acquire("child")
                with patch.object(gpu_queue, "ancestor_pids", return_value=[]), \
                        patch.object(gpu_queue, "holder_command", return_value=""), \
                        patch.object(gpu_queue.time, "sleep", side_effect=RuntimeError("waited")):
                    gpu_queue._write_holder(self.dir / "q", "replacement", "new-nonce")
                    with self.assertRaisesRegex(RuntimeError, "waited"):
                        gpu_queue.acquire("changed record")
                    gpu_queue._write_holder(self.dir / "q", "outer", info["nonce"])
                with patch.object(gpu_queue.os, "kill", side_effect=ProcessLookupError), \
                        patch.object(gpu_queue, "ancestor_pids", return_value=[]), \
                        patch.object(gpu_queue, "holder_command", return_value=""), \
                        patch.object(gpu_queue.time, "sleep", side_effect=RuntimeError("waited")):
                    with self.assertRaisesRegex(RuntimeError, "waited"):
                        gpu_queue.acquire("dead holder")
            finally:
                holder.release()
            self.assertEqual(os.environ[gpu_queue.HOLDER_ENV], "previous")

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
