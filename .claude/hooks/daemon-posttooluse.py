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
        # set), with the main session's id. The verdict computed from the
        # orchestrator's transcript must never be delivered to a subagent
        # (wrong context, and it'd mark the whisper consumed so the
        # orchestrator never sees it) — during orchestration most fires are
        # subagent fires, so without this the mis-delivery is the common
        # case (verified by probe, 2026-07-04).
        #
        # DESIGN.md §2b worker nudges (shipped OFF): behind
        # WORKER_NUDGES_FLAG, an agent-tagged event routes to that agent's
        # OWN mailbox (`<session>.<agent_id>`) instead of being skipped — a
        # second probe (2026-07-04, same method) confirmed additionalContext
        # returned from a subagent-tagged PostToolUse fire lands in the
        # SUBAGENT's own context, not the parent's, so this is safe once the
        # flag exists. With the flag absent (the only state so far — nobody
        # creates it), behavior is byte-for-byte what shipped before §2b.
        agent_id = data.get("agent_id")
        if agent_id and not valve.worker_nudges_enabled():
            return
        key = f"{session_id}.{agent_id}" if agent_id else session_id
        # DESIGN.md §2h.4: build_block's ack sentence needs agent_id too, so
        # a worker's self-grade line can carry it (RUNBOOK.md step 2 — plain
        # (session_id, seq) collides across workers).
        block, seq, move_id = valve.pending_injection(key, agent_id=agent_id)
        if not block:
            return
        valve.write_consumed(key, seq)
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": session_id,
                "agent_id": agent_id,
                "event": "injected",
                "valve": "PostToolUse",
                "seq": seq,
                "move_id": move_id,
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
