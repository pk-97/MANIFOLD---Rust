#!/usr/bin/env python3
"""permissions_sync_check.py — drift check between the live permission allow
lists and their reviewable copy in docs/PERMISSION_BOUNDARY.md §5.

settings.local.json is gitignored, so §5's fenced `permissions` block is the
only copy a reviewer (or a fresh session) can see. Hand transcription drifts —
the original §5 missed ten live rules on the day it was written, including the
two most dangerous (2026-07-26 audit). This script makes the block mechanical:

  - extracts the ```permissions block from the doc (one rule per line,
    `#` lines are comments),
  - reads the allow arrays from ~/.claude/settings.json,
    .claude/settings.json, .claude/settings.local.json,
  - diffs the two sets and exits 1 listing drift in both directions.

Run after any rule change. Exit 0 = in sync.
"""
import json
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DOC = os.path.join(REPO, "docs", "PERMISSION_BOUNDARY.md")
SETTINGS_FILES = [
    os.path.expanduser("~/.claude/settings.json"),
    os.path.join(REPO, ".claude", "settings.json"),
    os.path.join(REPO, ".claude", "settings.local.json"),
]


def doc_rules():
    text = open(DOC).read()
    m = re.search(r"```permissions\n(.*?)```", text, re.S)
    if not m:
        print("FAIL: no ```permissions block in docs/PERMISSION_BOUNDARY.md")
        sys.exit(1)
    return {
        line.strip()
        for line in m.group(1).splitlines()
        if line.strip() and not line.strip().startswith("#")
    }


def live_rules():
    rules = set()
    for path in SETTINGS_FILES:
        if not os.path.exists(path):
            continue
        with open(path) as f:
            data = json.load(f)
        rules.update(data.get("permissions", {}).get("allow", []))
    return rules


def main():
    documented = doc_rules()
    live = live_rules()
    missing_from_doc = sorted(live - documented)
    stale_in_doc = sorted(documented - live)
    if not missing_from_doc and not stale_in_doc:
        print(f"OK: {len(live)} allow rules in sync with PERMISSION_BOUNDARY.md §5")
        return 0
    if missing_from_doc:
        print("LIVE but NOT documented in §5:")
        for r in missing_from_doc:
            print(f"  + {r}")
    if stale_in_doc:
        print("Documented in §5 but NOT live:")
        for r in stale_in_doc:
            print(f"  - {r}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
