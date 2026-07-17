#!/usr/bin/env python3
"""SessionStart hook: loud context warning when the worktree pool is
oversized. Backstop for unknown-unknowns — the ring script and the
`git worktree add` deny should make this unreachable, but the 2026-07-15
incident (455 GB of worktrees, disk at 1.2 GB free) went unnoticed for
weeks precisely because nothing watched. stdout becomes session context.

Budgets: pool > POOL_WARN_GB, or more slot dirs than the ring cap.
Fail-silent: any error prints nothing (never blocks a session start).
"""
import subprocess
import sys
from pathlib import Path

POOL = Path(__file__).resolve().parents[2] / ".claude" / "worktrees"
POOL_WARN_GB = 200
MAX_SLOTS = 10  # keep in sync with scripts/agent-worktree.py


def main() -> int:
    try:
        if not POOL.is_dir():
            return 0
        dirs = [p for p in POOL.iterdir() if (p / ".git").exists()]
        out = subprocess.run(["du", "-sk", str(POOL)],
                             capture_output=True, text=True, timeout=60)
        size_gb = int(out.stdout.split()[0]) / 2**20 if out.returncode == 0 else 0
        problems = []
        if size_gb > POOL_WARN_GB:
            problems.append(f"the worktree pool is {size_gb:.0f} GB "
                            f"(budget {POOL_WARN_GB} GB)")
        if len(dirs) > MAX_SLOTS:
            problems.append(f"{len(dirs)} worktree dirs exist "
                            f"(ring cap {MAX_SLOTS})")
        if problems:
            print(
                "WORKTREE POOL OVER BUDGET: " + " and ".join(problems) + ". "
                "This should be structurally impossible (slot ring + git "
                "worktree add deny) — something bypassed the ring. Tell "
                "Peter NOW, run `python3 scripts/agent-worktree.py list`, "
                "and clean up idle slots with `git worktree remove` before "
                "other work. Incident precedent: 2026-07-15, 455 GB."
            )
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
