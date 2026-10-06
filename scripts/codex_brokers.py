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
rollout is still being written (a job marked completed can still be editing).

Plugin layout this mirrors (codex plugin scripts/lib/state.mjs,
broker-lifecycle.mjs): state dir = <root>/<slug>-<sha256(realpath(workspace))[:16]>,
root = $CLAUDE_PLUGIN_DATA/state or <tmpdir>/codex-companion; broker.json holds
endpoint, pidFile, logFile, sessionDir, pid; state.json holds jobs.

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
import sys
import tempfile
import time
from pathlib import Path

PLUGIN_DATA_DEFAULT = Path.home() / ".claude" / "plugins" / "data" / "codex-openai-codex"
CODEX_SESSIONS = Path.home() / ".codex" / "sessions"
ROLLOUT_FRESH_S = 120     # a rollout written this recently means Codex is mid-turn
RECENT_JOB_S = 6 * 3600   # only recent jobs can still be writing
SHUTDOWN_TIMEOUT_S = 3.0
EXIT_WAIT_S = 5.0


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
    workspace = Path(workspace)
    slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", workspace.name).strip("-") or "workspace"
    digest = hashlib.sha256(str(workspace.resolve()).encode()).hexdigest()[:16]
    return f"{slug}-{digest}"


def state_dirs(workspace):
    name = state_dir_name(workspace)
    return [root / name for root in state_roots() if (root / name).is_dir()]


def pid_alive(pid):
    try:
        os.kill(int(pid), 0)
        return True
    except PermissionError:
        return True
    except (OSError, TypeError, ValueError):
        return False


def _parse_iso(stamp):
    try:
        return calendar.timegm(time.strptime(stamp[:19], "%Y-%m-%dT%H:%M:%S"))
    except (TypeError, ValueError):
        return None


def rollout_fresh(thread_id, now):
    if not thread_id or not CODEX_SESSIONS.is_dir():
        return False
    for path in CODEX_SESSIONS.glob(f"*/*/*/rollout-*-{thread_id}.jsonl"):
        try:
            if now - path.stat().st_mtime < ROLLOUT_FRESH_S:
                return True
        except OSError:
            return True
    return False


def busy_reason(state_dir, now=None):
    """Why this broker must stay up, or None. Unreadable state fails closed."""
    now = time.time() if now is None else now
    state_file = state_dir / "state.json"
    if not state_file.exists():
        return None
    try:
        jobs = json.loads(state_file.read_text()).get("jobs", [])
    except (OSError, ValueError, AttributeError):
        return f"unreadable {state_file}"
    for job in jobs:
        status = job.get("status")
        pid = job.get("pid")
        if status in ("queued", "running") and (pid is None or pid_alive(pid)):
            return f"job {job.get('id')} is {status}"
        updated = _parse_iso(job.get("updatedAt") or job.get("completedAt"))
        if (updated is None or now - updated < RECENT_JOB_S) and rollout_fresh(job.get("threadId"), now):
            return f"job {job.get('id')} rollout written in the last {ROLLOUT_FRESH_S}s"
    return None


def send_shutdown(sock_path):
    """True if the broker acknowledged. Same request the plugin's SessionEnd sends."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(SHUTDOWN_TIMEOUT_S)
        try:
            sock.connect(sock_path)
            sock.sendall(b'{"id":1,"method":"broker/shutdown","params":{}}\n')
            return bool(sock.recv(4096))
        except OSError:
            return False


def _wait_exit(pid):
    deadline = time.time() + EXIT_WAIT_S
    while time.time() < deadline:
        if not pid_alive(pid):
            return True
        time.sleep(0.1)
    return not pid_alive(pid)


def _clear_files(state_dir, broker):
    for key in ("pidFile", "logFile"):
        if broker.get(key):
            Path(broker[key]).unlink(missing_ok=True)
    endpoint = broker.get("endpoint") or ""
    if endpoint.startswith("unix:"):
        Path(endpoint[len("unix:"):]).unlink(missing_ok=True)
    if broker.get("sessionDir"):
        try:
            Path(broker["sessionDir"]).rmdir()
        except OSError:
            pass
    (state_dir / "broker.json").unlink(missing_ok=True)


def stop_idle(workspace, dry_run=False):
    """Stop every idle plugin broker for this workspace. Returns report lines."""
    lines = []
    for state_dir in state_dirs(workspace):
        broker_file = state_dir / "broker.json"
        if not broker_file.exists():
            continue
        try:
            broker = json.loads(broker_file.read_text())
        except (OSError, ValueError):
            lines.append(f"KEEP broker {state_dir.name}: unreadable broker.json")
            continue
        pid = broker.get("pid")
        alive = pid is not None and pid_alive(pid)
        why = busy_reason(state_dir)
        if alive and why:
            lines.append(f"KEEP broker pid {pid}: {why}")
            continue
        endpoint = broker.get("endpoint") or ""
        if dry_run:
            lines.append(f"WOULD STOP broker pid {pid} ({'alive' if alive else 'gone'})")
            continue
        if alive:
            if not endpoint.startswith("unix:") or not send_shutdown(endpoint[len("unix:"):]):
                lines.append(f"KEEP broker pid {pid}: did not answer broker/shutdown at {endpoint}")
                continue
            if not _wait_exit(pid):
                lines.append(f"KEEP broker pid {pid}: acknowledged shutdown but still running")
                continue
        _clear_files(state_dir, broker)
        lines.append(f"STOPPED broker pid {pid}" if alive else f"CLEARED stale broker record (pid {pid} gone)")
    return lines


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
