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

2026-07-15 (Peter's ruling — see .claude/hooks/daemon-stop.py's docstring):
the grade-backstop and observation-review-prompt housekeeping nags moved
here from the Stop hook. Neither is a detection — they're end-of-session
reminders — so neither should be able to block a turn from ending. They
deliver as additionalContext on the next user prompt instead, with their
functions, sentinels, and reason wording carried over unchanged (only the
delivery mechanism changed: no more "decision": "block", just
additionalContext, and no per-turn stopblock sentinel — the existing
once-per-session sentinel files already give the "ask once" behavior). Only
one thing is delivered per prompt, same priority order as the old Stop
hook's sections: a pending mailbox flag first, then the grade backstop,
then the observation prompt.
"""
import glob
import json
import os
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

# ---- grade-backstop (2026-07-05 review, moved here 2026-07-15): self-grades
# can't be joined to fires when sessions never write them at all.
# Deterministic — main-session only (UserPromptSubmit carries no agent_id,
# same scope note as the pending-flag delivery below), fires at most ONCE
# per session (its own sentinel). No moves.md entry: this is valve plumbing,
# not a drift move, so the reminder text is authored directly here. ----
GRADEABLE_MOVE_PREFIXES = ("anchor/", "coaching/", "escalate/")
GRADE_BACKSTOP_STALE_EVENTS = 40  # matches §2d's oscillation-span convention
GRADE_BACKSTOP_MOVE_ID = "mechanical/grade-backstop"

# ---- observation review prompt (Peter, 2026-07-05; moved here 2026-07-15):
# a standing invitation to log anything worth the next sleep pass's
# attention, asked at most ONCE per session (own sentinel, mirroring the
# grade backstop) — most sessions have nothing to add, and asking every turn
# until something gets written would force busywork just to go quiet. Also
# gated on a minimum amount of session activity — a session a couple of tool
# calls long hasn't had a "session" worth reviewing. No moves.md entry
# (valve plumbing, not a drift move). ----
OBSERVATION_PROMPT_MOVE_ID = "mechanical/observation-prompt"
OBSERVATION_PROMPT_MIN_EVENTS = 40


def _session_gradeable_fires(telemetry_path, session_id, agent_id=None):
    """(seq, move_id, ts) for every gradeable (anchor/coaching/escalate) fire
    delivered to THIS mailbox — the session's own (agent_id=None, same scope
    as §4b's scoring) — oldest first. Reads telemetry.jsonl directly rather
    than needing a new field; never raises."""
    fires = []
    try:
        with open(telemetry_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if rec.get("event") != "injected" or rec.get("agent_id") != agent_id:
                    continue
                if rec.get("session_id") != session_id:
                    continue
                move_id = rec.get("move_id") or ""
                if not move_id.startswith(GRADEABLE_MOVE_PREFIXES):
                    continue
                if rec.get("seq") is None:
                    continue
                fires.append((rec["seq"], move_id, rec.get("ts")))
    except OSError:
        return []
    fires.sort(key=lambda t: t[0])
    return fires


def _session_grade_count(eval_dir, session_id, agent_id=None):
    """How many grade lines (any file matching live_grades*.jsonl) this
    mailbox — session-level (agent_id=None) — has already written. A coarse
    backstop count, not the precise per-fire join slice_fires.py does for the
    sleep pass. Records with no "agent_id" key at all read as agent_id=None
    via .get, matching every pre-§2h.4 session self-grade line on disk."""
    count = 0
    for path in glob.glob(os.path.join(eval_dir, "live_grades*.jsonl")):
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if rec.get("session_id") != session_id:
                        continue
                    if rec.get("agent_id") != agent_id:
                        continue
                    count += 1
        except OSError:
            continue
    return count


def _events_since(transcript_path, since_ts):
    """Count tool_result blocks (the same 'event' unit common.py's
    WindowState counts — one completed tool call) whose containing message
    postdates `since_ts`. Reads the transcript directly so this needs no new
    telemetry field."""
    if since_ts is None:
        return 0
    import common

    count = 0
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    d = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if d.get("type") != "user":
                    continue
                ts = common.parse_ts(d.get("timestamp"))
                if ts is not None and ts <= since_ts:
                    continue
                content = (d.get("message") or {}).get("content")
                if not isinstance(content, list):
                    continue
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        count += 1
    except OSError:
        return count
    return count


def _grade_backstop_reason(ungraded_count, oldest_events_ago, agent_id=None):
    agent_note = (
        f" Since this fire belongs to a worker, also pass --agent-id {agent_id} "
        f"— (session_id, seq) alone collides across workers."
        if agent_id
        else ""
    )
    return (
        f'<daemon move="{GRADE_BACKSTOP_MOVE_ID}">\n'
        f"This session delivered {ungraded_count} gradeable daemon fire(s) "
        f"(anchor/coaching/escalate) with no self-grade recorded yet, and the "
        f"oldest is {oldest_events_ago} tool events old. Before the session "
        f"ends, log one self-grade per ungraded fire, one shot each: "
        f"python3 .claude/daemon/log_grade.py <seq> <move_id> "
        f'<correct y/n> <effective y/n> "<one-sentence evidence>" — use each '
        f"fire's own seq so the sleep pass can join the grade back to the "
        f"exact fire it belongs to.{agent_note}\n"
        f"</daemon>"
    )


def _observation_prompt_reason(agent_id=None):
    agent_note = f', "agent_id": "{agent_id}"' if agent_id else ""
    return (
        f'<daemon move="{OBSERVATION_PROMPT_MOVE_ID}">\n'
        f"Before this session goes further: is there anything worth logging "
        f"for the daemon — a drift the observer should have caught but "
        f"didn't, a pattern that doesn't fit an existing move, or any other "
        f"note for the next sleep pass? Most sessions won't have anything, "
        f"and that's fine — no action needed either way. If something IS "
        f"worth logging, append one record to "
        f".claude/daemon/eval/observations.session.jsonl: {{ts, session_id, "
        f'kind: "miss-candidate"|"note", move_id or expect_family, evidence, '
        f"note{agent_note}}} (schema in RUNBOOK.md step 2/3). This asks once "
        f"per session, not every turn.\n"
        f"</daemon>"
    )


def _try_grade_backstop(verdicts_dir, telemetry_path, eval_dir, session_id, agent_id, transcript_path, stale_threshold):
    """One (reason, telemetry_extra) pair if the grade backstop should fire
    for this mailbox right now, else None. Never raises (caught by main()'s
    top-level try/except)."""
    mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id
    sentinel = os.path.join(verdicts_dir, f"{mailbox_key}.grade-backstop-fired")
    if os.path.exists(sentinel):
        return None
    fires = _session_gradeable_fires(telemetry_path, session_id, agent_id)
    if not fires:
        return None
    graded = _session_grade_count(eval_dir, session_id, agent_id)
    if len(fires) <= graded:
        return None
    events_ago = _events_since(transcript_path, fires[0][2])
    if events_ago <= stale_threshold:
        return None
    try:
        os.makedirs(verdicts_dir, exist_ok=True)
        open(sentinel, "w").close()
    except OSError:
        pass
    return (
        _grade_backstop_reason(len(fires) - graded, events_ago, agent_id),
        {
            "move_id": GRADE_BACKSTOP_MOVE_ID,
            "ungraded_count": len(fires) - graded,
            "oldest_events_ago": events_ago,
        },
    )


def _try_observation_prompt(verdicts_dir, session_id, agent_id, transcript_path, min_events):
    """One (reason, telemetry_extra) pair if the observation-review prompt
    should fire for this mailbox right now, else None. Never raises."""
    mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id
    sentinel = os.path.join(verdicts_dir, f"{mailbox_key}.observation-prompt-fired")
    if os.path.exists(sentinel):
        return None
    if _events_since(transcript_path, 0) < min_events:
        return None
    try:
        os.makedirs(verdicts_dir, exist_ok=True)
        open(sentinel, "w").close()
    except OSError:
        pass
    return _observation_prompt_reason(agent_id), {"move_id": OBSERVATION_PROMPT_MOVE_ID}


def _emit(context, extra, valve, session_id):
    valve.append_telemetry(
        {
            "ts": time.time(),
            "session_id": session_id,
            "agent_id": None,  # UserPromptSubmit is session-only (DESIGN.md §2b routes PostToolUse only)
            "event": "injected",
            "valve": "UserPromptSubmit",
            **extra,
        }
    )
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": context,
                }
            }
        )
    )


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

        block, seq, move_id = valve.pending_injection(session_id)
        if block:
            valve.write_consumed(session_id, seq)
            _emit(block, {"seq": seq, "move_id": move_id}, valve, session_id)
            return

        # Grade-backstop and observation-review-prompt (moved here 2026-07-15
        # from the Stop hook): housekeeping nags, not detections, so they
        # deliver as additionalContext rather than blocking a turn. Only one
        # thing per prompt — grade backstop takes priority, same ordering the
        # old Stop hook used.
        transcript_path = data.get("transcript_path")
        if transcript_path and os.path.exists(transcript_path):
            result = _try_grade_backstop(
                valve.VERDICTS_DIR, valve.TELEMETRY_PATH, valve.EVAL_DIR, session_id, None,
                transcript_path, GRADE_BACKSTOP_STALE_EVENTS,
            )
            if result:
                reason, extra = result
                _emit(reason, extra, valve, session_id)
                return

            result = _try_observation_prompt(
                valve.VERDICTS_DIR, session_id, None, transcript_path, OBSERVATION_PROMPT_MIN_EVENTS
            )
            if result:
                reason, extra = result
                _emit(reason, extra, valve, session_id)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
