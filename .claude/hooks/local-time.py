#!/usr/bin/env python3
"""SessionStart hook: inject the current Sydney wall-clock time as context.

Why (2026-07-29, Peter): sessions only get a bare date from the harness,
while transcripts, git internals, and most logs carry UTC timestamps — a
session reasoning about "last night" from UTC put an overnight wave on the
wrong day. Peter lives and performs in Sydney; every time in prose should
be Sydney local. This prints the authoritative local clock once at session
start so no session derives it from UTC artifacts.

Fail-silent: a clock line is a nice-to-have, never blocks a session.

Obsolete when: the harness injects local time + timezone itself.
"""
import subprocess
import sys
import time


def main() -> int:
    try:
        tz = subprocess.run(
            ["readlink", "/etc/localtime"], capture_output=True, text=True
        ).stdout.strip().split("zoneinfo/")[-1] or "unknown"
        now = time.strftime("%A %Y-%m-%d %H:%M %Z (UTC%z)")
        print(
            f"LOCAL TIME: {now} — system timezone {tz}. Peter is in Sydney; "
            "all times in prose are Sydney local. Session transcripts, git "
            "internal dates, and most logs store UTC — convert before "
            "reasoning about 'last night' or 'this morning'."
        )
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
