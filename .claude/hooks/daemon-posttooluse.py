#!/usr/bin/env python3
"""PostToolUse hook: the daemon's mid-turn valve.

Fires on every tool call — the main delivery point in autonomous runs
(DESIGN.md §2). Reads the observer daemon's verdict file (cheap: one stat +
one small JSON read, no model call); if a flag is pending and undelivered,
injects it as additional context tagged <daemon unvalidated> per the
2026-07-03 supervised go-live amendment, logs the injection to
telemetry.jsonl, and marks it consumed. Fails open on any error.
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
        # Revive the observer if it idle-exited — session activity is the
        # heartbeat; catchup rebuilds its state from the transcript.
        # (Subagent tool calls carry the MAIN session's transcript_path, so
        # reviving from one is correct.)
        valve.ensure_observer(session_id, data.get("transcript_path"))
        # This hook also fires for tool calls made INSIDE subagents (agent_id
        # set), with the main session's id. The verdict is computed from the
        # orchestrator's transcript — delivering here would inject it into the
        # wrong context AND mark it consumed, so the orchestrator never sees
        # it. During orchestration most fires are subagent fires, so without
        # this guard mis-delivery is the common case (verified by probe,
        # 2026-07-04).
        if data.get("agent_id"):
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
