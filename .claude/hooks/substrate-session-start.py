#!/usr/bin/env python3
"""SessionStart hook: spawn the substrate observer daemon, detached.

Fires on every source (startup, resume, clear, compact) — safe, because
observer.py itself guards against a duplicate spawn via a pidfile keyed on
session_id; a live daemon for this session just exits immediately. Never
blocks session start: any failure here is swallowed and logged nowhere,
per DESIGN.md invariant 1 (fail open, always).
"""
import json
import os
import subprocess
import sys

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
SUBSTRATE_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "substrate"))
VERDICTS_DIR = os.path.join(SUBSTRATE_DIR, "verdicts")


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return
    session_id = data.get("session_id")
    transcript_path = data.get("transcript_path")
    if not session_id or not transcript_path:
        return

    observer = os.path.join(SUBSTRATE_DIR, "observer.py")
    if not os.path.exists(observer):
        return

    try:
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        log_path = os.path.join(VERDICTS_DIR, f"{session_id}.log")
        with open(log_path, "a", encoding="utf-8") as log:
            subprocess.Popen(
                [sys.executable, observer, "--session-id", session_id, "--transcript", transcript_path],
                stdout=log,
                stderr=log,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                cwd=SUBSTRATE_DIR,
            )
    except Exception:
        pass


if __name__ == "__main__":
    main()
    sys.exit(0)
