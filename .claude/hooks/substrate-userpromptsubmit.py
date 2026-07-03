#!/usr/bin/env python3
"""UserPromptSubmit hook: the substrate's turn-start valve.

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
SUBSTRATE_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "substrate"))
sys.path.insert(0, SUBSTRATE_DIR)


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        if not session_id:
            return
        block, seq = valve.pending_injection(session_id)
        if not block:
            return
        valve.write_consumed(session_id, seq)
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": session_id,
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
