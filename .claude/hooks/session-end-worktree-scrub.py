#!/usr/bin/env python3
"""SessionEnd hook: trim the worktree pool when a session finishes.

acquire's target cap is lazy — it wipes only the slot it hands out — so an overnight
wave can leave every slot's warm cargo target on disk. The end of a session is the
natural teardown point: scrub wipes over-cap targets and then trims least-recently-built
idle caches until the pool is under its budget. Busy slots (leased, dirty, unlanded,
hosting a live session) are never touched, so a lane finishing while the lead still
works cannot pull caches out from under anyone.

Policy and constants (SCRUB_TO_GB, TARGET_CAP_GB) live in scripts/agent-worktree.py —
this hook only invokes it. Fail-silent: pool hygiene must never block a session from
ending.

Obsolete when: the ring script gains its own daemon/timer, or the pool moves off local
disk.
"""
import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "agent-worktree.py"


def main() -> int:
    try:
        out = subprocess.run([sys.executable, str(SCRIPT), "scrub"],
                             capture_output=True, text=True, timeout=300)
        if out.stdout:
            print(out.stdout.strip())
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
