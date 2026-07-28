#!/usr/bin/env python3
"""lane-report-enforcer.py — TeammateIdle hook: no lane goes idle silently.

Problem (Peter 2026-07-25, CRITICAL for team workflow): a teammate lane's
plain-text turn output is INVISIBLE to the lead — only explicit SendMessage
calls are delivered. Lanes that finish without calling SendMessage report
into the void; the lead sees only a contentless idle notification and must
read transcripts off disk.

Mechanism: TeammateIdle can BLOCK a teammate from going idle (exit 2 sends
stderr back to the teammate as feedback). This hook reads the lane's
transcript; if the lane has made no SendMessage tool call since its last
received message, the hook blocks the idle with feedback ordering it to
deliver its report via SendMessage to team-lead (or a one-line "nothing to
report" if it was told to stop). After MAX_BLOCKS consecutive blocks for
the same teammate, the hook allows the idle and emits a loud systemMessage
so the failure is visible to the user instead of looping forever.

Payload note: TeammateIdle's input fields are undocumented as of
2026-07-25 — the hook logs every payload to
/tmp/teammate_idle_payload_last.json for empirical verification, and fails
OPEN (allows the idle) on any error, so a guard bug never wedges a team.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the two-tier lead/lane model this guard polices; recheck at each routing-policy revision.
"""
import json
import os
import sys

MAX_BLOCKS = 3
# Anthropic-model lanes over-report; style advice doesn't hold, a bounce does.
# One re-write demand per idle, then allow (never loop on a stubborn lane).
MAX_REPORT_CHARS = 3000
PAYLOAD_LOG = "/tmp/teammate_idle_payload_last.json"
STATE = "/tmp/lane_report_enforcer_state.json"


def load_json(path, default):
    try:
        return json.load(open(path))
    except Exception:
        return default


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return  # can't parse — fail open

    try:
        json.dump(payload, open(PAYLOAD_LOG, "w"), indent=1)
    except Exception:
        pass

    try:
        # Payload carries the PARENT transcript; the teammate's own transcript
        # lives in <parent_dir>/<session_id>/subagents/agent-a<name>-<hash>.jsonl
        # (verified empirically 2026-07-25 via the payload log).
        parent_tp = payload.get("transcript_path") or ""
        teammate_id = payload.get("teammate_name") or payload.get("teammate_id") or payload.get("agent_id") or "unknown"
        transcript_path = None
        if parent_tp:
            import glob
            session_dir = os.path.splitext(parent_tp)[0]
            candidates = glob.glob(os.path.join(session_dir, "subagents", f"agent-a{teammate_id}-*.jsonl"))
            if candidates:
                transcript_path = max(candidates, key=os.path.getmtime)
        if not transcript_path:
            return  # can't find the lane's transcript — fail open

        # Did the lane call SendMessage since its last inbound message?
        sent = False
        last_report_len = 0
        try:
            for line in open(transcript_path):
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                msg = d.get("message", {})
                if msg.get("role") != "assistant":
                    continue
                for c in msg.get("content") or []:
                    if isinstance(c, dict) and c.get("type") == "tool_use" and c.get("name") in ("SendMessage", "mcp__team__SendMessage"):
                        sent = True
                        body = (c.get("input") or {}).get("message") or (c.get("input") or {}).get("content") or ""
                        last_report_len = len(body) if isinstance(body, str) else 0
        except Exception:
            return  # unreadable transcript — fail open

        if sent:
            state = load_json(STATE, {})
            len_key = f"{teammate_id}:len_bounced"
            if last_report_len > MAX_REPORT_CHARS and not state.get(len_key):
                state[len_key] = 1
                json.dump(state, open(STATE, "w"))
                print(
                    f"lane-report-enforcer: your report is {last_report_len} chars — over the "
                    f"{MAX_REPORT_CHARS} cap. Re-send via SendMessage compressed to: outcome, "
                    "numbers/exit codes, blockers. No narration, no history, no options you "
                    "don't recommend. Then stop.",
                    file=sys.stderr,
                )
                sys.exit(2)
            state.pop(len_key, None)
            json.dump(state, open(STATE, "w"))
            return  # report delivered — allow idle

        state = load_json(STATE, {})
        blocks = int(state.get(teammate_id, 0))
        if blocks >= MAX_BLOCKS:
            # Stop blocking; make the failure LOUD instead of looping.
            print(json.dumps({
                "systemMessage": (
                    f"lane-report-enforcer: teammate '{teammate_id}' went idle "
                    f"{MAX_BLOCKS}x without a SendMessage report — allowed through; "
                    "its output is LOST to the lead. Check the lane's brief/discipline."
                )
            }))
            state[teammate_id] = 0
            json.dump(state, open(STATE, "w"))
            return

        state[teammate_id] = blocks + 1
        json.dump(state, open(STATE, "w"))
        print(
            "lane-report-enforcer: your plain-text output is INVISIBLE to the team lead — "
            "only SendMessage is delivered. Before going idle you MUST call SendMessage to "
            "team-lead with your report/result (or a one-line 'nothing to report' if you were "
            "told to stop). Do that now, then stop.",
            file=sys.stderr,
        )
        sys.exit(2)
    except Exception:
        return  # any failure — fail open


if __name__ == "__main__":
    main()
