#!/usr/bin/env python3
"""UserPromptSubmit hook: the daemon's turn-start valve.

A second delivery point alongside PostToolUse (DESIGN.md §2), for a flag
raised right as a turn ends with no further tool call to carry it — e.g.
right before the user's next message. Shares the same verdict file and
consumed-seq marker as the PostToolUse valve (see valve.py), so whichever
fires first delivers it and this one is a no-op if PostToolUse already did.
Runs alongside the existing response-style and conversation-recall
UserPromptSubmit hooks — Claude Code merges additionalContext from all of
them. Fails open on any error.
"""
import json
import os
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        if not session_id:
            return
        # Revive the observer if it idle-exited — a new prompt after a long
        # break is exactly when observation must be back.
        valve.ensure_observer(session_id, data.get("transcript_path"))
        block, seq = valve.pending_injection(session_id)
        if not block:
            return
        valve.write_consumed(session_id, seq)
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": session_id,
                "agent_id": None,  # UserPromptSubmit is session-only (DESIGN.md §2b routes PostToolUse only)
                "event": "injected",
                "valve": "UserPromptSubmit",
                "seq": seq,
            }
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
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
