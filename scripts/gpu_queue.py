#!/usr/bin/env python3
"""Machine-wide GPU queue: one GPU run at a time, across every agent and worktree.

    scripts/gpu_queue.py [--label TEXT] -- <command...>   run under the lock
    scripts/gpu_queue.py status                           who holds it, since when

Concurrent GPU load gives flaky black renders and AGX faults, so every GPU run
waits its turn. The lock is an flock on `~/.cache/manifold/gpu.lock` (outside
every worktree; `MANIFOLD_GPU_QUEUE_DIR` overrides the directory). The kernel
drops it when the holder dies, however it dies. The holder writes
`gpu.holder` beside it so a waiter can say who it is waiting for.

The same lock is taken in-process by `manifold_gpu::queue` the first time a
`GpuDevice` is created (tests, headless renders, examples), so a raw
`cargo test --features gpu-proofs` queues even when nobody wrapped it. A GPU
process whose ancestor holds the lock (this wrapper running cargo running a
test binary) does not wait for it again: on contention it checks whether the
holder's pid is one of its own ancestors.

Importable: `with gpu_queue.hold("label"): ...` for scripts that take the lock
for a whole multi-process run. Waiting is poll-based, not first-come
first-served.

Obsolete when: the Vulkan backend ships its own device-level scheduler, or
GPU runs move off this machine.
"""

import argparse
import contextlib
import fcntl
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

POLL_SECONDS = 0.25
REPORT_SECONDS = 30.0

# Same-process nesting: `hold()` inside `hold()` must not block on itself.
_held_depth = 0


def queue_dir():
    override = os.environ.get("MANIFOLD_GPU_QUEUE_DIR")
    return Path(override) if override else Path.home() / ".cache" / "manifold"


def read_holder(directory):
    """The holder record as a dict, or {} when absent or unreadable."""
    info = {}
    try:
        text = (directory / "gpu.holder").read_text()
    except OSError:
        return info
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            info[key] = value
    return info


def _write_holder(directory, label):
    one_line = " ".join(f"{label}".split())[:300]
    cwd = " ".join(os.getcwd().split())
    body = f"pid={os.getpid()}\nsince={time.time():.3f}\nlabel={one_line}\ncwd={cwd}\n"
    tmp = directory / f"gpu.holder.tmp.{os.getpid()}"
    tmp.write_text(body)
    os.replace(tmp, directory / "gpu.holder")


def ancestor_pids():
    """Pids of this process's ancestors, nearest first (empty if ps fails)."""
    pids = []
    pid = os.getpid()
    for _ in range(64):
        try:
            out = subprocess.run(
                ["ps", "-o", "ppid=", "-p", str(pid)],
                capture_output=True, text=True, timeout=5,
            ).stdout.strip()
            pid = int(out)
        except (OSError, ValueError, subprocess.SubprocessError):
            break
        if pid <= 1:
            break
        pids.append(pid)
    return pids


def _describe(info):
    if not info:
        return "an unknown holder"
    try:
        age = format_age(time.time() - float(info.get("since", "")))
    except ValueError:
        age = "unknown time"
    return (f"pid {info.get('pid', '?')} `{info.get('label', '?')}` "
            f"in {info.get('cwd', '?')}, running for {age}")


def format_age(seconds):
    seconds = max(0, int(seconds))
    if seconds < 60:
        return f"{seconds}s"
    if seconds < 3600:
        return f"{seconds // 60}m{seconds % 60:02d}s"
    return f"{seconds // 3600}h{seconds % 3600 // 60:02d}m"


class Held:
    """What `acquire` returns: the open lock fd, or None when an ancestor holds it."""

    def __init__(self, fd, directory):
        self.fd = fd
        self.directory = directory

    def release(self):
        if self.fd is None:
            return
        # Clear our record before unlocking so a waiter never reads a record
        # for a lock nobody holds. A crash skips this; the record is then
        # stale until the next holder overwrites it, which is harmless
        # because waiters only read it while the lock is busy.
        with contextlib.suppress(OSError):
            (self.directory / "gpu.holder").unlink()
        os.close(self.fd)
        self.fd = None


def acquire(label, directory=None, poll=POLL_SECONDS, report=REPORT_SECONDS, out=None):
    """Block until this process may use the GPU. Returns a `Held`."""
    out = out or sys.stderr
    directory = Path(directory) if directory else queue_dir()
    directory.mkdir(parents=True, exist_ok=True)
    fd = os.open(directory / "gpu.lock", os.O_RDWR | os.O_CREAT, 0o666)
    started = time.monotonic()
    last_report = None
    ancestors = None
    try:
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                pass
            info = read_holder(directory)
            if ancestors is None:
                ancestors = set(ancestor_pids())
            holder_pid = info.get("pid", "")
            if holder_pid.isdigit() and int(holder_pid) in ancestors:
                os.close(fd)
                return Held(None, directory)
            now = time.monotonic()
            if last_report is None or now - last_report >= report:
                verb = "waiting for the GPU" if last_report is None else "still waiting for the GPU"
                print(f"[gpu-queue] {verb}: held by {_describe(info)}", file=out, flush=True)
                last_report = now
            time.sleep(poll)
    except BaseException:
        os.close(fd)
        raise
    if last_report is not None:
        print(f"[gpu-queue] acquired after {format_age(time.monotonic() - started)}",
              file=out, flush=True)
    _write_holder(directory, label)
    return Held(fd, directory)


@contextlib.contextmanager
def hold(label, **kwargs):
    """Hold the GPU lock for the body; re-entrant within and across processes."""
    global _held_depth
    if _held_depth:
        _held_depth += 1
        try:
            yield
        finally:
            _held_depth -= 1
        return
    held = acquire(label, **kwargs)
    _held_depth = 1
    try:
        yield
    finally:
        _held_depth = 0
        held.release()


def run_queued(command, label=None, **kwargs):
    """Run `command` while holding the lock; returns its exit code."""
    label = label or " ".join(command)
    with hold(label, **kwargs):
        try:
            child = subprocess.Popen(command)
        except OSError as err:
            print(f"gpu_queue: cannot run {command[0]}: {err}", file=sys.stderr)
            return 127

        def forward(signum, _frame):
            with contextlib.suppress(OSError):
                child.send_signal(signum)

        previous = {s: signal.signal(s, forward)
                    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        try:
            return child.wait()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)


def status(directory=None, out=None):
    out = out or sys.stdout
    directory = Path(directory) if directory else queue_dir()
    directory.mkdir(parents=True, exist_ok=True)
    fd = os.open(directory / "gpu.lock", os.O_RDWR | os.O_CREAT, 0o666)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(f"GPU busy: {_describe(read_holder(directory))}", file=out)
            return 1
        print("GPU free", file=out)
        return 0
    finally:
        os.close(fd)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv and argv[0] == "status":
        return status()
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--label", help="shown to waiters (default: the command)")
    parser.add_argument("command", nargs=argparse.REMAINDER,
                        help="`-- <command...>` to run under the lock, or `status`")
    args = parser.parse_args(argv)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("give a command after `--`, or `status`")
    return run_queued(command, label=args.label)


if __name__ == "__main__":
    sys.exit(main())
