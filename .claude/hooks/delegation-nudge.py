#!/usr/bin/env python3
"""delegation-nudge.py — PreToolUse hook that pressures the lead to delegate
once a session turns into sustained hands-on grinding (Peter 2026-07-28: the
lead ran a 15-probe render loop itself while the routing doctrine says lanes
run loops; "you NEVER listen to that rule").

Mechanism (deterministic, count-based, no content heuristics):
  - Counts EVERY hands-on action per session: Bash commands and
    Edit/Write/MultiEdit file edits.
  - An Agent tool call marks the session as delegating: it resets the window.
  - Every NUDGE_EVERY (20) consecutive hands-on actions with no Agent call in
    between, inject additionalContext telling the lead to stop and consider a
    lane. Injection repeats every further NUDGE_EVERY actions, so a long
    grind gets nudged at 20, 40, 60, ...

This hook only nudges — CLAUDE.md's default is "write code directly in the
main context" for normal-sized work, so denying all sustained direct work
would fight the contract. The DENY arm for instrument-probe loops lives in
probe-loop-guard.py. Fails OPEN on any error.

Obsolete when: docs/AGENT_ROUTING.md's lead/lane split is retired, or the
harness itself meters lead token spend against delegation.
"""
import json
import re
import sys

NUDGE_EVERY = 20

HANDS_ON_TOOLS = ("Bash", "Edit", "Write", "MultiEdit")


def state_path(session: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", session)[:64] or "unknown"
    return f"/tmp/manifold_delegation_{safe}.json"


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool = payload.get("tool_name", "")
        session = payload.get("session_id", "unknown")
        sp = state_path(session)

        state = {"hands_on": 0}
        try:
            state = json.load(open(sp))
        except Exception:
            pass

        if tool == "Agent":
            # Delegation observed — the window restarts from here.
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

        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": (
                    f"DELEGATION NUDGE — {n} hands-on actions (Bash/edits) in this "
                    "session without a single Agent spawn. Stop and route "
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
