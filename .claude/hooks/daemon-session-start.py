#!/usr/bin/env python3
"""SessionStart hook: spawn the daemon observer, detached, and deliver the
mechanical/reasoning-primer payload as additionalContext.

Fires on every source (startup, resume, clear, compact) — safe, because
observer.py itself guards against a duplicate spawn via a pidfile keyed on
session_id; a live daemon for this session just exits immediately. Never
blocks session start: any failure here is swallowed and logged nowhere,
per DESIGN.md invariant 1 (fail open, always).

2026-07-15 (Peter's ruling): mechanical/reasoning-primer moved here from the
observer's priming tier (see observer.py's removed `_check_primer`) — firing
on the first live tool event landed too early in the turn to be useful.
Delivery now happens exactly once per session start, unconditionally (no
mailbox, no cooldown, no seq — this is a static injection, not a fire that
needs consuming), using the SAME frozen `<daemon-advice>` wrapper
`valve.build_block` already produces for every other advice-kind move, so
the wording stays identical to what the priming tier used to emit
(DESIGN.md invariant 5: payload wording is frozen). Fails open like every
daemon hook: any error here means no additionalContext, never a blocked
session start.
"""
import json
import os
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

REASONING_PRIMER_MOVE_ID = "mechanical/reasoning-primer"


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        valve.ensure_observer(session_id, data.get("transcript_path"))

        block = valve.build_block({"move_id": REASONING_PRIMER_MOVE_ID})
        if not block:
            return
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": session_id,
                "agent_id": None,
                "event": "injected",
                "move_id": REASONING_PRIMER_MOVE_ID,
                "channel": "session_start",
            }
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": block,
                    }
                }
            )
        )
    except Exception:
        pass


if __name__ == "__main__":
    main()
    sys.exit(0)
