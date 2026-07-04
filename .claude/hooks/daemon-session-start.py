#!/usr/bin/env python3
"""SessionStart hook: spawn the daemon observer, detached.

Fires on every source (startup, resume, clear, compact) — safe, because
observer.py itself guards against a duplicate spawn via a pidfile keyed on
session_id; a live daemon for this session just exits immediately. Never
blocks session start: any failure here is swallowed and logged nowhere,
per DESIGN.md invariant 1 (fail open, always).
"""
import json
import os
import sys

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        valve.ensure_observer(data.get("session_id"), data.get("transcript_path"))
    except Exception:
        pass


if __name__ == "__main__":
    main()
    sys.exit(0)
