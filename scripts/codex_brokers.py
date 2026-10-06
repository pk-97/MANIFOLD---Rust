#!/usr/bin/env python3
"""Stop idle Codex plugin brokers that keep worktree slots pinned.

The Codex plugin starts one broker per workspace it runs a job in: a detached
node process whose cwd is that workspace, plus a `codex app-server` child and
its tool servers. The plugin's SessionEnd hook only stops the broker for the
session's own cwd, so a broker started with `-C <slot>` outlives the session.
The ring's live-session check (an lsof cwd scan) then reads those processes as
a session inside the slot and never hands the slot out again.

This module stops such a broker the way the plugin does: the `broker/shutdown`
request on its socket, which closes its app-server client, unlinks its socket
and pid file, and exits. It never signals a process. A broker is left alone
while any of its jobs is queued or running, or while a recent job's Codex
rollout shows an open turn: the plugin marks a job completed when Codex sends
a mid-turn message, and Codex can then stay silent for minutes inside a build.

Plugin layout this mirrors (codex plugin scripts/lib/state.mjs, workspace.mjs,
broker-lifecycle.mjs): state dir = <root>/<slug>-<sha256(realpath(toplevel))[:16]>,
root = $CLAUDE_PLUGIN_DATA/state or <tmpdir>/codex-companion; broker.json holds
endpoint, pidFile, logFile, sessionDir, pid; state.json holds jobs. Rollouts
are ~/.codex/sessions/Y/M/D/rollout-*-<threadId>.jsonl with event_msg turn
markers task_started, task_complete and turn_aborted.

Usage: scripts/codex_brokers.py [--dry-run] WORKTREE...

Obsolete when: the plugin's SessionEnd stops every broker its session started.
"""

import argparse
import calendar
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PLUGIN_DATA_DEFAULT = Path.home() / ".claude" / "plugins" / "data" / "codex-openai-codex"
CODEX_SESSIONS = Path.home() / ".codex" / "sessions"
BROKER_SCRIPT = "app-server-broker.mjs"
ROLLOUT_FRESH_S = 120     # backstop for a rollout with no turn markers yet
RECENT_JOB_S = 6 * 3600   # older jobs cannot still be mid-turn
ACK_GRACE_S = 600         # a broker that acknowledged and is still exiting is not asked again
SHUTDOWN_TIMEOUT_S = 3.0
EXIT_WAIT_S = 5.0
TAIL_CHUNK = 1 << 16
ACK_MARKER = ".shutdown-sent"
# Index 0 opens a turn; the others close it.
TURN_MARKERS = (b'"type":"event_msg","payload":{"type":"task_started"',
                b'"type":"event_msg","payload":{"type":"task_complete"',
                b'"type":"event_msg","payload":{"type":"turn_aborted"')


def state_roots():
    roots = []
    env = os.environ.get("CLAUDE_PLUGIN_DATA")
    for base in ([Path(env)] if env else []) + [PLUGIN_DATA_DEFAULT]:
        root = base / "state"
        if root not in roots:
            roots.append(root)
    roots.append(Path(tempfile.gettempdir()) / "codex-companion")
    return roots


def state_dir_name(workspace):
    # git --show-toplevel resolves symlinks, so the slug and the hash both come
    # from the resolved path.
    root = Path(workspace).resolve()
    slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", root.name).strip("-") or "workspace"
    return f"{slug}-{hashlib.sha256(str(root).encode()).hexdigest()[:16]}"


def state_dirs(workspace):
    name = state_dir_name(workspace)
    return [root / name for root in state_roots() if (root / name).is_dir()]


def pid_alive(pid):
    if type(pid) is not int or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


def broker_pid_alive(pid):
    """A recorded pid is the broker only while it runs the broker script:
    broker.json survives a reboot that hands the pid to something else."""
    if not pid_alive(pid):
        return False
    try:
        out = subprocess.run(["ps", "-o", "command=", "-p", str(pid)],
                             capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return True
    return BROKER_SCRIPT in out.stdout


def _parse_iso(stamp):
    try:
        return calendar.timegm(time.strptime(stamp[:19], "%Y-%m-%dT%H:%M:%S"))
    except (TypeError, ValueError):
        return None


def turn_open(path):
    """Is the last turn marker in this rollout a start? Read from the end."""
    with path.open("rb") as stream:
        end = stream.seek(0, os.SEEK_END)
        tail = b""
        while end > 0:
            start = max(0, end - TAIL_CHUNK)
            stream.seek(start)
            tail = stream.read(end - start) + tail
            end = start
            pos, kind = max((tail.rfind(marker), i) for i, marker in enumerate(TURN_MARKERS))
            if pos >= 0:
                return kind == 0
    return False


def rollout_busy(thread_id, now):
    if not isinstance(thread_id, str) or not re.fullmatch(r"[0-9A-Za-z-]+", thread_id):
        return None
    for path in CODEX_SESSIONS.glob(f"*/*/*/rollout-*-{thread_id}.jsonl"):
        try:
            if turn_open(path):
                return "turn still open in its Codex rollout"
            if now - path.stat().st_mtime < ROLLOUT_FRESH_S:
                return f"rollout written in the last {ROLLOUT_FRESH_S}s"
        except OSError:
            return "rollout unreadable"
    return None


def busy_reason(state_dir, now=None):
    """Why this broker must stay up, or None. Unreadable state fails closed."""
    now = time.time() if now is None else now
    state_file = state_dir / "state.json"
    if not state_file.exists():
        return None
    try:
        state = json.loads(state_file.read_text())
    except (OSError, ValueError):
        return f"unreadable {state_file}"
    jobs = state.get("jobs", []) if isinstance(state, dict) else None
    if not isinstance(jobs, list) or not all(isinstance(job, dict) for job in jobs):
        return f"unreadable {state_file}"
    for job in jobs:
        updated = _parse_iso(job.get("updatedAt") or job.get("completedAt"))
        recent = updated is None or now - updated < RECENT_JOB_S
        if job.get("status") in ("queued", "running"):
            pid = job.get("pid")
            if recent and (pid is None or pid_alive(pid)):
                return f"job {job.get('id')} is {job.get('status')}"
        if recent:
            why = rollout_busy(job.get("threadId"), now)
            if why:
                return f"job {job.get('id')}: {why}"
    return None


def send_shutdown(sock_path):
    """'acked', 'gone' (nothing listening), or 'silent'. Same request the
    plugin's SessionEnd sends."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(SHUTDOWN_TIMEOUT_S)
        try:
            sock.connect(sock_path)
        except (FileNotFoundError, ConnectionRefusedError):
            return "gone"
        except OSError:
            return "silent"
        try:
            sock.sendall(b'{"id":1,"method":"broker/shutdown","params":{}}\n')
            return "acked" if sock.recv(4096) else "silent"
        except OSError:
            return "silent"


def _wait_exit(pid):
    if not broker_pid_alive(pid):
        return True
    deadline = time.time() + EXIT_WAIT_S
    while time.time() < deadline:
        if not pid_alive(pid):
            return True
        time.sleep(0.1)
    return not pid_alive(pid)


def _clear_files(state_dir, broker, sock_path):
    for key in ("pidFile", "logFile"):
        if broker.get(key):
            Path(broker[key]).unlink(missing_ok=True)
    Path(sock_path).unlink(missing_ok=True)
    if broker.get("sessionDir"):
        try:
            Path(broker["sessionDir"]).rmdir()
        except OSError:
            pass
    (state_dir / ACK_MARKER).unlink(missing_ok=True)
    (state_dir / "broker.json").unlink(missing_ok=True)


def _stop_one(state_dir, dry_run, now):
    try:
        broker = json.loads((state_dir / "broker.json").read_text())
    except (OSError, ValueError):
        broker = None
    keys = ("endpoint", "pidFile", "logFile", "sessionDir")
    if not isinstance(broker, dict) or any(
            broker.get(k) is not None and not isinstance(broker[k], str) for k in keys):
        return f"KEEP broker {state_dir.name}: unreadable broker.json"
    pid = broker.get("pid")
    why = busy_reason(state_dir, now)
    if why:
        return f"KEEP broker pid {pid}: {why}"
    endpoint = broker.get("endpoint") or ""
    if not endpoint.startswith("unix:"):
        return f"KEEP broker pid {pid}: no unix endpoint ({endpoint!r})"
    sock_path = endpoint[len("unix:"):]
    marker = state_dir / ACK_MARKER
    if marker.exists() and now - marker.stat().st_mtime < ACK_GRACE_S and broker_pid_alive(pid):
        return f"KEEP broker pid {pid}: acknowledged shutdown earlier, still exiting"
    if dry_run:
        return f"WOULD STOP broker pid {pid}"
    result = send_shutdown(sock_path)
    if result == "silent":
        return f"KEEP broker pid {pid}: did not answer broker/shutdown at {endpoint}"
    if result == "gone":
        if broker_pid_alive(pid):
            return f"KEEP broker pid {pid}: socket gone but the broker runs; it cannot be asked to stop"
        _clear_files(state_dir, broker, sock_path)
        return f"CLEARED stale broker record (pid {pid} not a running broker)"
    if not _wait_exit(pid):
        marker.touch()
        return f"KEEP broker pid {pid}: acknowledged shutdown but still running"
    _clear_files(state_dir, broker, sock_path)
    return f"STOPPED broker pid {pid}"


def stop_idle(workspace, dry_run=False):
    """Stop every idle plugin broker for this workspace. Returns report lines."""
    now = time.time()
    return [_stop_one(state_dir, dry_run, now) for state_dir in state_dirs(workspace)
            if (state_dir / "broker.json").exists()]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("worktrees", nargs="+", type=Path)
    args = parser.parse_args()
    for wt in args.worktrees:
        for line in stop_idle(wt, dry_run=args.dry_run):
            print(f"{wt.name}: {line}")


if __name__ == "__main__":
    sys.exit(main())
