#!/usr/bin/env python3
"""SessionStart hook: loud context warning when worktrees are eating the disk.

Backstop for unknown-unknowns — the ring script and the `git worktree add` deny should
make this unreachable, but the 2026-07-15 incident (455 GB of worktrees, disk at 1.2 GB
free) went unnoticed for weeks because nothing watched. stdout becomes session context.

Checks: free disk below MIN_FREE_GB, or more slot dirs than the ring cap. Free space is
the harm itself and costs one statfs call; walking the pool with `du` cost ~5s per
session start.
Fail-silent: any error prints nothing (never blocks a session start).

Obsolete when: the ring script refuses to acquire below a free-space floor.
"""
import shutil
import sys
from pathlib import Path

POOL = Path(__file__).resolve().parents[2] / ".claude" / "worktrees"
MIN_FREE_GB = 40
MAX_SLOTS = 10  # keep in sync with scripts/agent-worktree.py


def main() -> int:
    try:
        if not POOL.is_dir():
            return 0
        dirs = [p for p in POOL.iterdir() if (p / ".git").exists()]
        free_gb = shutil.disk_usage(POOL).free / 2**30
        problems = []
        if free_gb < MIN_FREE_GB:
            problems.append(f"the disk has {free_gb:.0f} GB free (floor {MIN_FREE_GB} GB)")
        if len(dirs) > MAX_SLOTS:
            problems.append(f"{len(dirs)} worktree dirs exist (ring cap {MAX_SLOTS})")
        if problems:
            print(
                "WORKTREE POOL OVER BUDGET: " + " and ".join(problems) + ". "
                "Tell Peter, run `scripts/agent-worktree.py list`, then "
                "`scripts/agent-worktree.py scrub` (never raw `git worktree remove` — "
                "the Bash hook denies it). More slot dirs than the ring cap means "
                "something bypassed the ring. Precedent: 2026-07-15, 455 GB."
            )
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
