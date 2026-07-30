#!/usr/bin/env python3
"""SessionStart: surface stale open beads so the pile can't rot silently.

An open bead sitting untouched past threshold is abnormal, not "still queued" — the old
markdown backlog died by accumulating exactly these: items nobody chose to fix and
nobody chose to close. This hook is the forced choice.

Behavior: reads `bd list --json --flat` (open issues), staleness = days since
`updated_at`. Thresholds: P1 >= 7 days, P2/P3 >= 21. Prints at most the 5 oldest per
priority — a bounded list gets read, a full one gets ignored. Each surfaced item demands
one of three moves: fix it, demote it (with `bd update`), or close it with a reason.
Silent when nothing is stale.

Fails OPEN: any error (bd missing, JSON shape change) prints nothing and exits 0 —
session start must never wedge on housekeeping.

Obsolete when: beads is retired as the tracker, or bd grows a native staleness/triage
surface that the session sees without this hook.
"""
import json
import subprocess
import sys
from datetime import datetime, timezone

THRESHOLD_DAYS = {1: 7, 2: 21, 3: 21}
MAX_PER_PRIORITY = 5


def main():
    r = subprocess.run(
        ["bd", "list", "--json", "--flat"],
        capture_output=True, text=True, timeout=30,
    )
    if r.returncode != 0:
        return
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
        return

    lines = ["STALE BEADS — each one gets a verb this session: fix, demote "
             "(bd update <id> -p <n>), or close with a reason (bd close <id>). "
             "Ignoring the list is how the old backlog died."]
    for prio in sorted(stale):
        items = sorted(stale[prio], reverse=True)[:MAX_PER_PRIORITY]
        extra = len(stale[prio]) - len(items)
        lines.append(f"P{prio} (threshold {THRESHOLD_DAYS[prio]}d"
                     + (f", {extra} more not shown" if extra > 0 else "") + "):")
        for age, bid, title in items:
            lines.append(f"  {bid}  {age}d untouched  {title}")
    print("\n".join(lines))


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print(f"bead-staleness failed open: {e}", file=sys.stderr)
    sys.exit(0)
