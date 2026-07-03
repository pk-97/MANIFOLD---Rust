#!/usr/bin/env python3
"""PostToolUse hook: the substrate's mid-turn valve.

Fires on every tool call — the main delivery point in autonomous runs
(DESIGN.md §2). Reads the observer daemon's verdict file (cheap: one stat +
one small JSON read, no model call); if a flag is pending and undelivered,
injects it as additional context tagged <substrate unvalidated> per the
2026-07-03 supervised go-live amendment, logs the injection to
telemetry.jsonl, and marks it consumed. Fails open on any error.
"""
import json
import os
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
SUBSTRATE_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "substrate"))
sys.path.insert(0, SUBSTRATE_DIR)


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        if not session_id:
            return
        # Revive the observer if it idle-exited — session activity is the
        # heartbeat; catchup rebuilds its state from the transcript.
        valve.ensure_observer(session_id, data.get("transcript_path"))
        block, seq = valve.pending_injection(session_id)
        if not block:
            return
        valve.write_consumed(session_id, seq)
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": session_id,
                "event": "injected",
                "valve": "PostToolUse",
                "seq": seq,
            }
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": block,
                    }
                }
            )
        )
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
