#!/usr/bin/env python3
"""Fleet health: the stalls and blockers a lead otherwise finds by hand.

The lead's cron runs this every 15 minutes and acts on each line. It exits 1
when anything needs the lead, else 0. It does not see Claude lanes (Agent-tool
subagents); their stalls show only as orphaned slot edits. Checks:

- disk: free space below the cargo build guard's floor stops every lane, so
  it fails there and warns within DISK_WARN_MARGIN of it.
- slots: uncommitted edits untouched for ORPHAN_MINUTES that no running
  process names, which is finished work nobody picked up.
- GPU lock: held by a live pid for over 30 minutes (a hung proof).
- nightly: no recent trunk-health log, or a launchd log ending in a crash.
"""

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from storage_budget import GIB, MAINTENANCE_GOAL_BYTES as DISK_FLOOR  # noqa: E402  the build guard refuses below it

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
WORKTREES = MAIN_CHECKOUT / ".claude/worktrees"
CACHE = Path.home() / ".cache/manifold"

DISK_WARN_MARGIN = 15 * GIB
ORPHAN_MINUTES = 30
HANDOFF_FILES = {"WORKTREE_HANDOFF.md"}  # left uncommitted on purpose


def pid_alive(pid):
    if not pid:
        return False
    try:
        os.kill(int(pid), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OSError, ValueError):
        return False
    return True


def mtime(path):
    try:
        return path.stat().st_mtime
    except OSError:
        return None


def check_disk(problems):
    free = shutil.disk_usage(MAIN_CHECKOUT).free
    if free < DISK_FLOOR:
        problems.append(("FAIL", f"disk: {free / GIB:.0f} GiB free, under the {DISK_FLOOR // GIB} GiB build guard; every cargo run is refused. Clean merged slots' targets."))
    elif free < DISK_FLOOR + DISK_WARN_MARGIN:
        problems.append(("WARN", f"disk: {free / GIB:.0f} GiB free, near the {DISK_FLOOR // GIB} GiB build guard."))


def process_commands():
    try:
        out = subprocess.run(["ps", "-Ao", "command"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return ""
    return out


def dirty_paths(slot):
    try:
        out = subprocess.run(["git", "-C", str(slot), "status", "--porcelain", "-uall"],
                             capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    paths = []
    for line in out.splitlines():
        path = line[3:].split(" -> ")[-1].strip('"')
        if path and Path(path).name not in HANDOFF_FILES:
            paths.append(slot / path)
    return paths


def check_slots(now, problems):
    if not WORKTREES.is_dir():
        return
    procs = process_commands()
    for slot in sorted(WORKTREES.glob("slot-*")):
        paths = dirty_paths(slot)
        if not paths:
            continue
        newest = max((m for m in map(mtime, paths) if m), default=None)
        if newest is None or now - newest < ORPHAN_MINUTES * 60:
            continue
        if str(slot) in procs:
            continue
        problems.append(("ACT", f"{slot.name}: {len(paths)} uncommitted paths untouched for {(now - newest) / 60:.0f} min, no process on it. Review and commit, or hand it to a lane."))


def check_gpu_lock(problems):
    try:
        text = (CACHE / "gpu.holder").read_text()
    except OSError:
        return
    info = dict(line.partition("=")[::2] for line in text.splitlines() if "=" in line)
    pid = info.get("pid")
    if pid and not pid_alive(pid):
        return  # the flock is already free; the record is overwritten by the next holder
    since = float(info.get("since", "0") or 0)
    if pid and since and time.time() - since > 30 * 60:
        problems.append(("WARN", f"GPU lock: held {(time.time() - since) / 60:.0f} min by pid {pid} ({info.get('label', '?')[:80]}). Check it is not hung."))


NIGHTLY_DIR = MAIN_CHECKOUT / ".claude/orchestration/trunk-health"
NIGHTLY_STALE_HOURS = 36


def check_nightly(problems, now, log_dir=NIGHTLY_DIR):
    """A nightly that never starts leaves no red bead, so its silence is the only signal."""
    dated = sorted(log_dir.glob("[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].log"))
    if not dated:
        problems.append(("FAIL", f"trunk-health: no nightly log in {log_dir}. The nightly has never run."))
    else:
        age = (now - dated[-1].stat().st_mtime) / 3600
        if age > NIGHTLY_STALE_HOURS:
            problems.append(("FAIL", f"trunk-health: newest nightly log {dated[-1].name} is {age:.0f} h old. The nightly is not running."))
    try:
        tail = (log_dir / "launchd.log").read_text(errors="replace")[-4000:]
    except OSError:
        return
    last = tail.rfind("Traceback")
    if last != -1 and "[trunk-health] " not in tail[last:]:
        problems.append(("FAIL", "trunk-health: launchd.log ends in a crash (Traceback). The nightly died before reporting."))


def main(argv=None):
    argparse.ArgumentParser(description=__doc__.splitlines()[0]).parse_args(argv)
    now = time.time()
    problems = []
    check_disk(problems)
    check_slots(now, problems)
    check_gpu_lock(problems)
    check_nightly(problems, now)
    for level, line in problems:
        print(f"{level:4} {line}")
    print(f"fleet-health: {len(problems)} item(s)")
    return 1 if any(level in ("FAIL", "ACT") for level, _ in problems) else 0


if __name__ == "__main__":
    sys.exit(main())
