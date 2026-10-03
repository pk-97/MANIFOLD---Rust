#!/usr/bin/env python3
"""Nightly stale-beads check (a trunk_health.py gate, not a session hook).

An open bead sitting untouched past threshold is abnormal, not "still queued" — the old
markdown backlog died by accumulating exactly these: items nobody chose to fix and
nobody chose to close. This check is the forced choice, once a night instead of at
every session start.

Behavior: reads `bd list --json --flat` (open issues), staleness = days since
`updated_at`. Thresholds: P1 >= 7 days, P2/P3 >= 21. Prints one count line and the
five items that matter most (highest priority, then oldest). Each surfaced item
demands one of three moves: fix it, demote it (with `bd update`), or close it with a
reason. Exits 1 when anything is stale so trunk_health files the night's bead; silent
and green when nothing is.

Fails OPEN on tooling errors (bd missing, JSON shape change): prints the error and
exits 0 — housekeeping never costs the night's real gates.

Obsolete when: beads is retired as the tracker, or bd grows a native staleness/triage
surface that runs unattended.
"""
import json
import subprocess
import sys
from datetime import datetime, timezone

THRESHOLD_DAYS = {1: 7, 2: 21, 3: 21}
SHOWN = 5


def main() -> int:
    r = subprocess.run(
        ["bd", "list", "--json", "--flat"],
        capture_output=True, text=True, timeout=30,
    )
    if r.returncode != 0:
        print(f"stale_beads: bd list failed (exit {r.returncode}), skipped", file=sys.stderr)
        return 0
    issues = json.loads(r.stdout)
    now = datetime.now(timezone.utc)

    stale = {}  # priority -> [(age_days, id, title)]
    for it in issues:
        if it.get("status") == "closed":
            continue
        prio = it.get("priority")
        ts = it.get("updated_at") or it.get("created_at")
        if prio not in THRESHOLD_DAYS or not ts:
            continue
        try:
            updated = datetime.fromisoformat(ts.replace("Z", "+00:00"))
        except ValueError:
            continue
        age = (now - updated).days
        if age >= THRESHOLD_DAYS[prio]:
            stale.setdefault(prio, []).append(
                (age, it.get("id", "?"), (it.get("title") or "")[:80]))

    if not stale:
        print("stale_beads: nothing stale")
        return 0

    counts = ", ".join(f"P{p} {len(stale[p])}" for p in sorted(stale))
    top = [(p, *item) for p in sorted(stale) for item in sorted(stale[p], reverse=True)][:SHOWN]
    lines = [f"STALE BEADS ({counts}). Give each one below a verb: fix, "
             "demote (bd update <id> -p <n>), or close with a reason (bd close <id>)."]
    for prio, age, bid, title in top:
        lines.append(f"  P{prio} {bid}  {age}d  {title}")
    print("\n".join(lines))
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        print(f"stale_beads failed open: {e}", file=sys.stderr)
        sys.exit(0)
