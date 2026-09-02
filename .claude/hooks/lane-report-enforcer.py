#!/usr/bin/env python3
"""lane-report-enforcer.py — TeammateIdle hook: exactly one report, through one channel.

Harness 2.1.x auto-delivers a teammate lane's final plain-text turn to the parent
session as its completion report (verified 2026-09-02 on 2.1.258: the lead received
both trailing text turns of the scene-loop p1-fix lane as "Teammate finished"
notifications). The old SendMessage mandate — written 2026-07-25, when plain-text
output went nowhere — now delivers every report TWICE. With the dispatcher seat
retired (roster 2026-08-31) all lanes are lead-spawned, so the mandate inverts:

LEAD-parent lanes: the final text turn IS the report. Idle is blocked (exit 2 feeds
stderr back to the lane) when the lane's last assistant message carries no real text
(< MIN_REPORT_CHARS) — the feedback orders a final report (outcome, verdict-file
path, blockers) and forbids a SendMessage duplicate. A report over MAX_REPORT_CHARS
gets ONE compression bounce per lane, then allow — never loop on a stubborn lane.
A lane that SendMessaged a report AND ends with report text gets a loud
systemMessage: the double already happened, the brief is what to fix.

UNKNOWN-parent lanes (no lead-seat marker in the parent transcript head): the old
SendMessage mandate stays — a duplicate annoys, silence loses work.

After MAX_BLOCKS consecutive blocks for the same teammate the hook allows the idle
with a loud systemMessage so the failure is visible instead of looping forever.

Payload note: TeammateIdle's input fields are undocumented — the hook logs every payload
to /tmp/teammate_idle_payload_last.json for empirical verification, and fails OPEN
(allows the idle) on any error, so a guard bug never wedges a team.

Obsolete when: the harness stops auto-delivering a teammate's final text turn, or a
dispatcher seat is un-retired and the per-parent branches need re-verification;
recheck at each routing-policy revision.
"""
import glob
import json
import os
import sys

MIN_REPORT_CHARS = 25  # floor that passes "Nothing to report" but not "working on it"
MAX_REPORT_CHARS = 3000
MAX_BLOCKS = 3
PAYLOAD_LOG = "/tmp/teammate_idle_payload_last.json"
STATE = "/tmp/lane_report_enforcer_state.json"


def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return default


def find_transcript(parent_tp: str, teammate_id: str) -> str | None:
    """The teammate's transcript lives next to the parent's, under
    <parent_stem>/subagents/agent-a<name>-<hash>.jsonl (verified empirically
    2026-07-25 via the payload log)."""
    if not parent_tp:
        return None
    session_dir = os.path.splitext(parent_tp)[0]
    candidates = glob.glob(
        os.path.join(session_dir, "subagents", f"agent-a{teammate_id}-*.jsonl")
    )
    if not candidates:
        return None
    return max(candidates, key=os.path.getmtime)


def parent_is_lead(parent_tp: str) -> bool:
    """seat-identity.py injects 'lead seat' into the top session's context; the
    marker lands in the session transcript. No marker -> unknown parent -> the
    safe SendMessage mandate stays."""
    try:
        with open(parent_tp) as f:
            return "lead seat" in f.read(200_000).lower()
    except Exception:
        return False


def scan_lane(transcript_path: str) -> tuple[bool, int, str]:
    """(sent_a_message, last_message_chars, last_text) across assistant turns."""
    sent = False
    last_msg_len = 0
    last_text = ""
    try:
        with open(transcript_path) as f:
            for line in f:
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                msg = d.get("message", {})
                if msg.get("role") != "assistant":
                    continue
                for c in msg.get("content") or []:
                    if not isinstance(c, dict):
                        continue
                    if c.get("type") == "tool_use" and c.get("name") in (
                        "SendMessage",
                        "mcp__team__SendMessage",
                    ):
                        sent = True
                        body = (c.get("input") or {}).get("message") or ""
                        if not isinstance(body, str):
                            body = json.dumps(body)
                        last_msg_len = len(body)
                    elif c.get("type") == "text" and (c.get("text") or "").strip():
                        last_text = c["text"].strip()
    except Exception:
        pass
    return sent, last_msg_len, last_text


def decide(teammate_id: str, parent_tp: str, transcript_path: str, state: dict):
    """(block_feedback | None, system_message | None). Pure given the paths and
    state dict — main() wires stdin/stdout and persists state."""
    sent, last_msg_len, last_text = scan_lane(transcript_path)
    len_key = f"{teammate_id}:len_bounced"

    if parent_is_lead(parent_tp):
        if len(last_text) >= MIN_REPORT_CHARS:
            if len(last_text) > MAX_REPORT_CHARS and not state.get(len_key):
                state[len_key] = 1
                return (
                    f"lane-report-enforcer: your final report is {len(last_text)} chars — over the "
                    f"{MAX_REPORT_CHARS} cap. End again with it compressed to: outcome, "
                    "numbers/exit codes, blockers. No narration, no history, no options you "
                    "don't recommend. Do NOT SendMessage it — your final text turn is "
                    "auto-delivered to the lead. Then stop.",
                    None,
                )
            state.pop(len_key, None)
            if sent:
                return None, (
                    f"lane-report-enforcer: teammate '{teammate_id}' reported via SendMessage "
                    "AND a final text turn — the lead got it twice. Its brief must say: "
                    "report in the final text turn; SendMessage is mid-flight only."
                )
            return None, None

        blocks = int(state.get(teammate_id, 0))
        if blocks >= MAX_BLOCKS:
            state[teammate_id] = 0
            return None, (
                f"lane-report-enforcer: teammate '{teammate_id}' went idle {MAX_BLOCKS}x "
                "without a final-text report — allowed through; its outcome is LOST to "
                "the lead. Check the lane's brief/discipline."
            )
        state[teammate_id] = blocks + 1
        return (
            "lane-report-enforcer: your final plain-text turn IS your report — this harness "
            "auto-delivers it to the team lead. Do NOT SendMessage it (that delivers it "
            "twice). End with a real report — outcome, verdict-file path, blockers (or a "
            "one-line 'nothing to report' if you were told to stop) — then stop.",
            None,
        )

    # Unknown parent: old mandate — SendMessage is the only guaranteed channel.
    if sent:
        if last_msg_len > MAX_REPORT_CHARS and not state.get(len_key):
            state[len_key] = 1
            return (
                f"lane-report-enforcer: your report is {last_msg_len} chars — over the "
                f"{MAX_REPORT_CHARS} cap. Re-send via SendMessage compressed to: outcome, "
                "numbers/exit codes, blockers. Then stop.",
                None,
            )
        state.pop(len_key, None)
        return None, None

    blocks = int(state.get(teammate_id, 0))
    if blocks >= MAX_BLOCKS:
        state[teammate_id] = 0
        return None, (
            f"lane-report-enforcer: teammate '{teammate_id}' went idle {MAX_BLOCKS}x "
            "without a SendMessage report — allowed through; its output may be LOST to "
            "its parent seat. Check the lane's brief/discipline."
        )
    state[teammate_id] = blocks + 1
    return (
        "lane-report-enforcer: no SendMessage report seen from you. If your parent is a "
        "dispatcher seat it never sees your completion — SendMessage your report to it "
        "now, then stop. (If your parent is the lead, ignore this: your final text turn "
        "is auto-delivered.)",
        None,
    )


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
        parent_tp = payload.get("transcript_path") or ""
        teammate_id = (
            payload.get("teammate_name")
            or payload.get("teammate_id")
            or payload.get("agent_id")
            or "unknown"
        )
        transcript_path = find_transcript(parent_tp, teammate_id)
        if not transcript_path:
            return  # can't find the lane's transcript — fail open

        state = load_json(STATE, {})
        feedback, loud = decide(teammate_id, parent_tp, transcript_path, state)
        try:
            json.dump(state, open(STATE, "w"))
        except Exception:
            pass

        if feedback:
            print(feedback, file=sys.stderr)
            sys.exit(2)
        if loud:
            print(json.dumps({"systemMessage": loud}))
    except Exception:
        return  # any failure — fail open


if __name__ == "__main__":
    main()
