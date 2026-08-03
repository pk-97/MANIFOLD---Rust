#!/usr/bin/env python3
"""PreToolUse hook that pressures the lead to delegate once a session turns into sustained
hands-on grinding.

Mechanism (deterministic, count-based, no content heuristics):
  - Counts every hands-on action per session: Bash commands and Edit/Write/MultiEdit file edits.
  - An Agent tool call marks the session as delegating: it resets the window. A
    SendMessage call (driving a live lane) also resets — orchestrating is delegation.
  - Every NUDGE_EVERY (20) consecutive hands-on actions with no Agent call in between, inject additionalContext telling the lead to stop and consider a lane. Injection repeats every further NUDGE_EVERY actions.

This hook only nudges — CLAUDE.md's default is "write code directly in the main context"
for normal-sized work, so denying all sustained direct work would fight the contract.
The DENY arm for instrument-probe loops lives in probe-loop-guard.py. Fails OPEN on any
error.

LEAD SEAT ONLY: grinding is a lane's job description — nudging a worker to spawn agents
inverts the routing model. Seat test: subagent/teammate PreToolUse payloads carry
`agent_id`/`agent_type`; the lead's carry neither. Marker present -> silent. Never use
transcript-model detection for seats: teammate payloads carry the PARENT transcript, so
the model always reads as the lead.

Obsolete when: docs/AGENT_ROUTING.md's lead/lane split is retired, or the harness itself
meters lead token spend against delegation.
"""
import json
import re
import sys

NUDGE_EVERY = 20

HANDS_ON_TOOLS = ("Bash", "Edit", "Write", "MultiEdit")


def is_worker_seat(payload: dict) -> bool:
    return any(payload.get(k) for k in ("agent_id", "agent_type", "teammate_name"))


def state_path(session: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", session)[:64] or "unknown"
    return f"/tmp/manifold_delegation_{safe}.json"


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool = payload.get("tool_name", "")
        session = payload.get("session_id", "unknown")

        if is_worker_seat(payload):
            return  # worker seat — grinding is its job, never nudge it to spawn

        sp = state_path(session)

        state = {"hands_on": 0}
        try:
            state = json.load(open(sp))
        except Exception:
            pass

        if tool == "Agent":
            # Delegation observed — the window restarts from here.
            state["hands_on"] = 0
            state["spawns"] = int(state.get("spawns", 0)) + 1
            json.dump(state, open(sp, "w"))
            return

        if tool == "SendMessage":
            # Driving a live lane IS delegation work, not grinding.
            state["hands_on"] = 0
            json.dump(state, open(sp, "w"))
            return

        if tool not in HANDS_ON_TOOLS:
            return

        state["hands_on"] = int(state.get("hands_on", 0)) + 1
        json.dump(state, open(sp, "w"))
        n = state["hands_on"]

        if n % NUDGE_EVERY != 0:
            return

        spawns = int(state.get("spawns", 0))
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": (
                    f"DELEGATION NUDGE — {n} consecutive hands-on actions "
                    f"(Bash/edits) since the last delegation signal "
                    f"({spawns} Agent spawn(s) this session). Stop and route "
                    "(docs/AGENT_ROUTING.md, CLAUDE.md Agents): if what you are doing "
                    "is a decided, mechanical loop — probe renders, bulk edits, "
                    "run-test-fix-repeat, format sweeps — brief a lane "
                    "(cc-fleet subagent / Agent tool) and only judge its report. "
                    "Lead context is the scarcest resource in the rig. If the work "
                    "genuinely needs lead judgment at every step, continue — but say "
                    "so to the user in one line."
                ),
            }
        }))
    except Exception:
        # fail open — never block a session on a guard bug
        return


if __name__ == "__main__":
    main()
