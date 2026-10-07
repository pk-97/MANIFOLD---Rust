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
first-served. A landing announces itself with `landing_pending()` before it
builds; nightly callers use `hold(..., priority="nightly")` and yield to that
announcement between independent GPU runs.

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

from gate_policy import SLOW_THRESHOLD_S

POLL_SECONDS = 0.25
REPORT_SECONDS = 30.0
HOLDER_ENV = "MANIFOLD_GPU_LOCK_HOLDER"
LANDING_PENDING_DIR = "landing.pending"
NIGHTLY_WAITING = "nightly.waiting"
ADMISSION_LOCK = "gpu.admission"

# Same-process nesting: `hold()` inside `hold()` must not block on itself.
_held_depth = 0


def queue_dir():
    override = os.environ.get("MANIFOLD_GPU_QUEUE_DIR")
    return Path(override) if override else Path.home() / ".cache" / "manifold"


def _landing_pending_dir(directory):
    return Path(directory) / LANDING_PENDING_DIR


def _read_pending(path):
    info = {}
    try:
        text = path.read_text()
    except OSError:
        return info
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            info[key] = value
    return info


def process_start_identity(pid=None):
    """Return a stable process-start token, or ``None`` when it is unknown.

    A PID alone is unsafe for landing announcements: the kernel can recycle it
    after a landing dies.  Linux exposes a monotonic start tick in procfs;
    macOS exposes the start timestamp through libproc.  Keep this helper small
    and patchable so queue tests can exercise PID reuse deterministically.
    """
    pid = os.getpid() if pid is None else int(pid)
    try:
        stat = Path(f"/proc/{pid}/stat")
        if stat.exists():
            fields = stat.read_text().rsplit(") ", 1)[-1].split()
            if len(fields) > 19:
                return f"proc:{fields[19]}"
    except (OSError, ValueError):
        pass
    if sys.platform == "darwin":
        try:
            import ctypes

            class ProcBsdInfo(ctypes.Structure):
                _fields_ = [
                    ("flags", ctypes.c_uint32), ("status", ctypes.c_uint32),
                    ("xstatus", ctypes.c_uint32), ("pid", ctypes.c_uint32),
                    ("ppid", ctypes.c_uint32), ("uid", ctypes.c_uint32),
                    ("gid", ctypes.c_uint32), ("ruid", ctypes.c_uint32),
                    ("rgid", ctypes.c_uint32), ("svuid", ctypes.c_uint32),
                    ("svgid", ctypes.c_uint32), ("reserved", ctypes.c_uint32),
                    ("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32),
                    ("nfiles", ctypes.c_uint32), ("pgid", ctypes.c_uint32),
                    ("pjobc", ctypes.c_uint32), ("tdev", ctypes.c_uint32),
                    ("tpgid", ctypes.c_uint32), ("nice", ctypes.c_int32),
                    ("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64),
                ]

            libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
            proc_pidinfo = libproc.proc_pidinfo
            proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                     ctypes.c_void_p, ctypes.c_int]
            proc_pidinfo.restype = ctypes.c_int
            info = ProcBsdInfo()
            if proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) == ctypes.sizeof(info):
                return f"proc:{info.start_sec}.{info.start_usec}"
        except (OSError, AttributeError, TypeError):
            pass
    try:
        result = subprocess.run(["ps", "-o", "lstart=", "-p", str(pid)],
                                capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return None
    value = result.stdout.strip()
    return f"ps:{value}" if result.returncode == 0 and value else None


def _identity_live(info):
    """Whether a pending record still names the same process incarnation."""
    pid = info.get("pid", "")
    expected = info.get("start", "")
    if not pid.isdigit() or int(pid) <= 1 or not expected:
        return False
    try:
        os.kill(int(pid), 0)
    except PermissionError:
        pass  # A live process outside our signal permission still owns its record.
    except OSError:
        return False
    return process_start_identity(int(pid)) == expected


def pending_landings(directory=None):
    """Return live landing announcements, removing dead or malformed records."""
    directory = Path(directory) if directory else queue_dir()
    pending_dir = _landing_pending_dir(directory)
    try:
        paths = list(pending_dir.iterdir())
    except OSError:
        return []
    live = []
    for path in paths:
        if path.suffix != '.pending' or not path.is_file():
            continue
        info = _read_pending(path)
        if _identity_live(info):
            info["path"] = str(path)
            live.append(info)
        else:
            with contextlib.suppress(OSError):
                path.unlink()
    return live


def _pending_description(info):
    label = info.get("label", "landing")
    return f"pid {info.get('pid', '?')} `{label}`"


@contextlib.contextmanager
def landing_pending(directory=None, label="landing"):
    """Announce a live landing so nightly GPU work yields before its next leg."""
    directory = Path(directory) if directory else queue_dir()
    pending_dir = _landing_pending_dir(directory)
    pending_dir.mkdir(parents=True, exist_ok=True)
    nonce = secrets.token_hex(16)
    start = process_start_identity()
    if not start:
        raise RuntimeError("cannot identify landing process start")
    path = pending_dir / f"{os.getpid()}.{nonce}.pending"
    body = (f"pid={os.getpid()}\nstart={start}\nnonce={nonce}\nsince={time.time():.3f}\n"
            f"label={' '.join(str(label).split())[:300]}\n")
    tmp = pending_dir / f".{path.name}.tmp"
    tmp.write_text(body)
    os.replace(tmp, path)
    try:
        yield
    finally:
        with contextlib.suppress(OSError):
            path.unlink()


def _nightly_waiting_path(directory):
    return Path(directory) / NIGHTLY_WAITING


def _nightly_waiting(directory):
    """Read the fair-queue turn marker, removing a dead owner."""
    path = _nightly_waiting_path(directory)
    info = _read_pending(path)
    if not info:
        return {}
    if not _identity_live(info):
        with contextlib.suppress(OSError):
            path.unlink()
        return {}
    info["path"] = str(path)
    return info


def _batch_paths(info):
    return {path for path in info.get("batch", "").split("|") if path}


def _write_nightly_waiting(directory, batch):
    start = process_start_identity()
    if not start:
        raise RuntimeError("cannot identify nightly process start")
    path = _nightly_waiting_path(directory)
    body = (f"pid={os.getpid()}\nstart={start}\nsince={time.time():.3f}\n"
            f"batch={'|'.join(sorted(batch))}\n")
    tmp = directory / f".{path.name}.tmp.{os.getpid()}"
    tmp.write_text(body)
    os.replace(tmp, path)
    return path


def _remove_own_nightly_waiting(directory):
    with _admission_guard(directory):
        _remove_own_nightly_waiting_locked(directory)


def _remove_own_nightly_waiting_locked(directory):
    path = _nightly_waiting_path(directory)
    info = _read_pending(path)
    if _is_own_turn(info):
        with contextlib.suppress(OSError):
            path.unlink()


@contextlib.contextmanager
def _admission_guard(directory):
    """Serialize nightly turn selection without holding the GPU lock."""
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    fd = os.open(directory / ADMISSION_LOCK, os.O_RDWR | os.O_CREAT, 0o666)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        with contextlib.suppress(OSError):
            fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _own_landing_paths(directory):
    with _admission_guard(directory):
        return _own_landing_paths_locked(directory)


def _own_landing_paths_locked(directory):
    pid = str(os.getpid())
    start = process_start_identity()
    if not start:
        return set()
    return {info["path"] for info in pending_landings(directory)
            if info.get("pid") == pid and info.get("start") == start}


def _normal_waits_for_nightly(directory):
    """Block newer normal work while a nightly turn drains its landing batch."""
    with _admission_guard(directory):
        return _normal_waits_for_nightly_locked(directory)


def _is_own_turn(info):
    return (bool(info) and info.get("pid") == str(os.getpid())
            and info.get("start") == process_start_identity())


def _normal_waits_for_nightly_locked(directory):
    waiting = _nightly_waiting(directory)
    if not waiting:
        return False
    # A landing already in the frozen batch must be allowed to acquire and
    # drain; announcements after that snapshot wait for the nightly turn.
    return not (_own_landing_paths_locked(directory) & _batch_paths(waiting))


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


def acquire(label, directory=None, poll=POLL_SECONDS, report=REPORT_SECONDS, out=None,
            priority="normal"):
    """Block until this process may use the GPU. Returns a `Held`.

    ``priority="nightly"`` freezes the currently announced landing batch,
    admits one nightly leg after that batch drains, and defers announcements
    made after the snapshot. An active holder is never interrupted.
    """
    if priority not in {"normal", "nightly"}:
        raise ValueError(f"unknown GPU queue priority: {priority}")
    out = out or sys.stderr
    directory = Path(directory) if directory else queue_dir()
    directory.mkdir(parents=True, exist_ok=True)
    fd = os.open(directory / "gpu.lock", os.O_RDWR | os.O_CREAT, 0o666)
    started = time.monotonic()
    last_report = None
    ancestors = None
    try:
        while True:
            acquired = False
            busy_info = None
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                acquired = True
            except BlockingIOError:
                busy_info = read_holder(directory)
                # Check inherited ownership before applying queue fairness. A
                # cargo/test child can be a nightly-priority caller while its
                # landing parent already owns the lock; making it wait here
                # would deadlock the parent behind its child.
                inherited = _token_matches(busy_info)
                holder_pid = busy_info.get("pid", "")
                if not inherited and holder_pid.isdigit():
                    if ancestors is None:
                        ancestors = set(ancestor_pids())
                    inherited = int(holder_pid) in ancestors
                if inherited:
                    os.close(fd)
                    return Held(None, directory)

            with _admission_guard(directory):
                waiting = _nightly_waiting(directory)
                allowed = True
                if priority == "nightly":
                    if waiting and not _is_own_turn(waiting):
                        allowed = False
                    elif not waiting:
                        pending = pending_landings(directory)
                        _write_nightly_waiting(directory,
                                               {info["path"] for info in pending
                                                if info.get("path")})
                        waiting = _nightly_waiting(directory)
                    if waiting and _is_own_turn(waiting):
                        pending = pending_landings(directory)
                        allowed = not any(info["path"] in _batch_paths(waiting)
                                          for info in pending if info.get("path"))
                elif waiting:
                    allowed = not _normal_waits_for_nightly_locked(directory)

                if allowed and acquired:
                    if priority == "nightly":
                        _remove_own_nightly_waiting_locked(directory)
                    break

            if acquired:
                fcntl.flock(fd, fcntl.LOCK_UN)
            if busy_info is not None:
                now = time.monotonic()
                if last_report is None or now - last_report >= report:
                    verb = "waiting for the GPU" if last_report is None else "still waiting for the GPU"
                    print(f"[gpu-queue] {verb}: held by {_describe(busy_info)}",
                          file=out, flush=True)
                    last_report = now
            time.sleep(poll)
    except BaseException:
        with _admission_guard(directory):
            if priority == "nightly":
                _remove_own_nightly_waiting_locked(directory)
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
def hold(label, priority="normal", **kwargs):
    """Hold the GPU lock for the body; re-entrant within and across processes."""
    global _held_depth
    if _held_depth:
        _held_depth += 1
        try:
            yield
        finally:
            _held_depth -= 1
        return
    held = acquire(label, priority=priority, **kwargs)
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
    import gate_passes
    passed = gate_passes.queued_proof(command, Path.cwd()) if kind == 'test' else None
    if kind == 'test' and any('gpu-proofs' in arg for arg in command) and not passed and not _cargo_has_no_run(command):
        print('[NO REUSE] gpu-proofs: command is outside the canonical serial proof grammar',
              flush=True)
    if passed and passed.reused():
        return 0
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
    if passed and gate_passes.changed_passes([passed]):
        # The binary was planned and built against an earlier content
        # snapshot. Never admit that stale plan to the GPU; the caller must
        # replan and rebuild against the changed inputs.
        print("[NO REUSE] gpu-proofs: inputs changed after build planning; "
              "refusing stale GPU admission", flush=True)
        return 2
    with hold(label, **kwargs):
        if passed and gate_passes.changed_passes([passed]):
            print('[NO REUSE] gpu-proofs: inputs changed while waiting for the GPU', flush=True)
            return 2
        started = time.monotonic()
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
            code = child.wait()
            if passed:
                seconds = time.monotonic() - started
                if code == 0 and seconds > SLOW_THRESHOLD_S:
                    # Raw cargo has no per-test watchdog/timings. A total
                    # over the heavy-test threshold cannot earn a reusable
                    # receipt without the instrumented timing proof.
                    print('[NO REUSE] gpu-proofs: raw cargo duration cannot establish '
                          'per-test hang allowances or reviewed heavy-test timing; '
                          'use gpu_proofs_gate.py', flush=True)
                else:
                    passed.save(code, seconds)
            return code
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
