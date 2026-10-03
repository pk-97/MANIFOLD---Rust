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
test binary) does not wait for it again: on contention it first validates the
inherited MANIFOLD_GPU_LOCK_HOLDER=<pid>:<nonce> against gpu.holder and a live
holder PID. Each acquisition writes a fresh nonce and exports the token until
release. Changed records and dead holders invalidate tokens. Without a valid
token, the ancestor PID walk remains a fallback for unwrapped processes.

When this wrapper is given `cargo test` or `cargo run`, Cargo compilation is
completed before the GPU lock is acquired. `cargo test` receives `--no-run`
before its libtest `--` separator; `cargo run` is prebuilt with the matching
`cargo build` command. A direct `cargo build` is also completed without taking
the lock. Unsupported or ambiguous Cargo invocations fail before any lock is
taken.

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
import secrets
import subprocess
import sys
import time
from pathlib import Path

POLL_SECONDS = 0.25
REPORT_SECONDS = 30.0
HOLDER_ENV = "MANIFOLD_GPU_LOCK_HOLDER"

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


def _write_holder(directory, label, nonce):
    one_line = " ".join(f"{label}".split())[:300]
    cwd = " ".join(os.getcwd().split())
    body = f"pid={os.getpid()}\nnonce={nonce}\nsince={time.time():.3f}\nlabel={one_line}\ncwd={cwd}\n"
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


def _token_matches(info):
    """Accept only a matching per-acquisition token naming a live holder."""
    pid, nonce = info.get("pid", ""), info.get("nonce", "")
    if not pid.isdigit() or int(pid) <= 1 or not nonce:
        return False
    if os.environ.get(HOLDER_ENV) != f"{pid}:{nonce}":
        return False
    try:
        os.kill(int(pid), 0)
    except OSError:
        return False
    return True


def _describe(info):
    if not info:
        return "an unknown holder"
    try:
        age = format_age(time.time() - float(info.get("since", "")))
    except ValueError:
        age = "unknown time"
    command = holder_command(info.get("pid", ""))
    shown = f" [{command}]" if command else ""
    return (f"pid {info.get('pid', '?')} `{info.get('label', '?')}`{shown} "
            f"in {info.get('cwd', '?')}, holding for {age}")


def holder_command(pid):
    """The holder's live command line (truncated), or "" when it cannot be read."""
    if not str(pid).isdigit():
        return ""
    try:
        out = subprocess.run(["ps", "-o", "command=", "-p", str(pid)],
                             capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.SubprocessError):
        return ""
    return " ".join(out.split())[:160]


def format_age(seconds):
    seconds = max(0, int(seconds))
    if seconds < 60:
        return f"{seconds}s"
    if seconds < 3600:
        return f"{seconds // 60}m{seconds % 60:02d}s"
    return f"{seconds // 3600}h{seconds % 3600 // 60:02d}m"


class Held:
    """What `acquire` returns: the open lock fd, or None when an ancestor holds it."""

    def __init__(self, fd, directory, previous_token=None):
        self.fd = fd
        self.directory = directory
        self.previous_token = previous_token

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
        if self.previous_token is None:
            os.environ.pop(HOLDER_ENV, None)
        else:
            os.environ[HOLDER_ENV] = self.previous_token


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
            if _token_matches(info):
                os.close(fd)
                return Held(None, directory)
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
    nonce = secrets.token_hex(16)
    previous_token = os.environ.get(HOLDER_ENV)
    try:
        _write_holder(directory, label, nonce)
        os.environ[HOLDER_ENV] = f"{os.getpid()}:{nonce}"
    except BaseException:
        os.close(fd)
        raise
    return Held(fd, directory, previous_token)


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


class UnsupportedCargoInvocation(ValueError):
    """A Cargo command whose build/run phases cannot be derived safely."""


_CARGO_GLOBAL_FLAGS = {
    "-q", "--quiet", "-v", "--verbose", "--locked", "--offline", "--frozen",
}
_CARGO_GLOBAL_OPTIONS = {"--color", "--config", "--root"}


def _cargo_subcommand_index(command):
    """Return the supported Cargo subcommand's index in ``command``."""
    if not command or Path(command[0]).name != "cargo":
        return None
    separator_indexes = [index for index, arg in enumerate(command) if arg == "--"]
    if len(separator_indexes) > 1:
        raise UnsupportedCargoInvocation("Cargo command has more than one `--` separator")
    end = separator_indexes[0] if separator_indexes else len(command)
    args = command[1:end]
    index = 0
    if args[:1] and args[0].startswith("+"):
        index = 1  # Cargo toolchain selector, e.g. `+nightly`.
    while index < len(args):
        arg = args[index]
        if arg in _CARGO_GLOBAL_FLAGS:
            index += 1
            continue
        if arg in _CARGO_GLOBAL_OPTIONS:
            if index + 1 >= len(args):
                raise UnsupportedCargoInvocation(f"Cargo option {arg} needs a value")
            index += 2
            continue
        if any(arg.startswith(option + "=") for option in _CARGO_GLOBAL_OPTIONS):
            index += 1
            continue
        if arg.startswith("-"):
            raise UnsupportedCargoInvocation(f"cannot identify Cargo subcommand after {arg}")
        if arg in {"test", "run", "build"}:
            return index + 1  # ``args`` starts after the executable.
        raise UnsupportedCargoInvocation(f"unsupported Cargo subcommand: {arg}")
    raise UnsupportedCargoInvocation("cannot identify Cargo subcommand")


def _cargo_kind(command):
    """Return the supported Cargo subcommand, or None for a non-Cargo command."""
    index = _cargo_subcommand_index(command)
    return command[index] if index is not None else None


def _cargo_build_command(command, kind):
    """Derive the lock-free Cargo build for a supported invocation."""
    separator = next((index for index, arg in enumerate(command) if arg == "--"), None)
    if kind == "test":
        head = list(command if separator is None else command[:separator])
        if "--no-run" not in head:
            head.append("--no-run")
        if separator is None:
            return head
        return head + command[separator:]
    if kind == "run":
        if separator is None:
            head = list(command)
        else:
            head = list(command[:separator])
        subcommand = _cargo_subcommand_index(head)
        head[subcommand] = "build"
        return head
    if kind == "build":
        if separator is not None:
            raise UnsupportedCargoInvocation("Cargo build cannot have a runtime separator")
        return list(command)
    raise AssertionError(f"unhandled Cargo subcommand: {kind}")


def _run_build(command):
    """Run a lock-free Cargo build and return its exit code."""
    try:
        return subprocess.run(command).returncode
    except OSError as err:
        print(f"gpu_queue: cannot run {command[0]}: {err}", file=sys.stderr)
        return 127


def _ancestor_holds(directory=None):
    """Whether a recorded holder is an ancestor of this process.

    An empty ancestor list with a holder record is treated as unknown and
    therefore unsafe for a lock-free build. This keeps a nested invocation
    from compiling under a lock when process inspection is unavailable.
    """
    directory = Path(directory) if directory else queue_dir()
    info = read_holder(directory)
    holder_pid = info.get("pid", "")
    if not holder_pid.isdigit():
        return False
    if _token_matches(info):
        return True
    ancestors = ancestor_pids()
    return not ancestors or int(holder_pid) in ancestors


def _cargo_has_no_run(command):
    separator = next((index for index, arg in enumerate(command) if arg == "--"), len(command))
    return "--no-run" in command[:separator]


def run_queued(command, label=None, **kwargs):
    """Build Cargo commands before the lock, then run the GPU phase under it."""
    label = label or " ".join(command)
    try:
        kind = _cargo_kind(command)
        build_command = _cargo_build_command(command, kind) if kind else None
    except UnsupportedCargoInvocation as err:
        print(f"gpu_queue: {err}", file=sys.stderr)
        return 2
    if build_command is not None and (_held_depth or _ancestor_holds(kwargs.get("directory"))):
        print("gpu_queue: cannot prebuild Cargo while the GPU lock is already held",
              file=sys.stderr)
        return 2
    if build_command is not None:
        build_code = _run_build(build_command)
        if build_code:
            return build_code
        if kind == "build" or (kind == "test" and _cargo_has_no_run(command)):
            return 0
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
