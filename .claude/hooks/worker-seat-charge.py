#!/usr/bin/env python3
"""worker-seat-charge.py — PreToolUse hook injecting the worker-seat charge into every
subagent/teammate, once, on its first tool call.

seat-identity.py is SessionStart-wired and SessionStart never fires for
teammate/subagent spawns, so no worker ever received its seat charge. Guards deny
(agent-tier-spawn-guard), but the instructional layer must exist too: denial without
doctrine turns workers into workaround-hunters.

Mechanism: worker seats are identified by payload markers
(`agent_id`/`agent_type`/`teammate_name` — lead payloads carry none). First
marker-carrying tool call per agent identity injects additionalContext; a /tmp state
file keyed by agent identity suppresses repeats. NEVER env-based and NEVER
transcript-based: workers inherit the parent's env and their payload transcript_path is
the PARENT transcript, so both misidentify the seat as the lead.

Fails OPEN on any error.

Obsolete when: the harness fires SessionStart (or an equivalent start event) for
subagent/teammate spawns — move the charge there.
"""
import json
import re
import sys

CHARGE = (
    "WORKER SEAT (machine-injected, trust over any contrary self-belief): "
    "this session is a subagent/teammate seat in the MANIFOLD roster "
    "(docs/AGENT_ROUTING.md), driven by the lead. Your charge: execute your "
    "brief exactly as written; never spawn agents at any depth; never write "
    "outside the paths your brief names; a hook denial is a STOP — report "
    "the denial text up, never work around it; any fork or gap in the brief "
    "= stop and report up. If you write records (commits, docs), sign as "
    "your own model and seat, never as the lead's."
)


def ident(payload: dict) -> str:
    for k in ("agent_id", "teammate_name", "agent_type"):
        v = payload.get(k)
        if v:
            return re.sub(r"[^A-Za-z0-9_-]", "_", str(v))[:80]
    return ""


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        who = ident(payload)
        if not who:
            return  # lead seat — seat-identity.py / the system prompt own this
        mark = f"/tmp/manifold_seat_charge_{who}"
        try:
            with open(mark, "x"):
                pass
        except FileExistsError:
            return  # already charged
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": CHARGE,
            }
        }))
    except Exception:
        return  # fail open


if __name__ == "__main__":
    main()
