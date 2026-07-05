#!/usr/bin/env python3
"""Stop hook: the daemon's turn-end valve (DESIGN.md §2 "Known delivery gap",
RULED 2026-07-04, spec confirmed against the current hooks reference).

A flag raised on a turn's FINAL assistant text has no delivery channel until
the next human prompt — PostToolUse never fires again that turn, and that is
exactly where verify-claim's most common firing position (done-claims are
turn-final) lands one turn late. This hook may block the Stop event ONCE,
delivering a pending whisper as the block reason so the model gets one beat
to self-correct before yielding.

Re-ruled 2026-07-05 (Peter): the classifier race is no longer accepted —
turn-final corrections landing on the NEXT prompt defeated the point. The
hook now waits (bounded) for the observer to CATCH UP: the observer
publishes a `.offset` heartbeat after each drain, classification runs
synchronously inside the drain, so heartbeat >= transcript-size-at-Stop
means every window this turn produced has been judged and any verdict is
on disk. The wait is capped (STOP_WAIT_CAP_S), skipped entirely when the
observer is dead or pre-heartbeat (fail open), and delivers the instant a
verdict appears. Typical cost is one observer poll (~1s); the cap only
binds when a classification is genuinely in flight — exactly the turns
the whisper is for.

A second, purely deterministic check (never the classifier — moves.md's
mechanical/announced-not-started) catches a turn whose final text announces
imminent action ("Starting X now", "Beginning X", "Let me now...") with no
tool call following it in the turn.

Guard: "block at most once per turn" is enforced two ways — a
`<session>.stopblock.<prompt_id>` sentinel file (the durable guard; a turn is
one prompt_id, regardless of which mailbox supplied the whisper) and the
`stop_hook_active` stdin field if the running Claude Code version sets it
(undocumented in the current hooks reference as of 2026-07-04, but free
defense-in-depth if present). Either one present means this turn already
spent its one block.

Fails open on every error; never raises, never leaves the process non-zero.
"""
import json
import os
import re
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

STOPBLOCK_MAX_AGE = 24 * 60 * 60  # sentinels older than a day are stale, sweep them

# Grade-backstop (2026-07-05 review): self-grades can't be joined to fires
# when sessions never write them at all. Deterministic, mirrors the
# announced-not-started check below — main-session only, fires at most ONCE
# per session (its own sentinel, not the per-turn stopblock, since nagging
# every turn until the backlog clears would defeat "before the session
# ends"). No moves.md entry: this is valve plumbing, not a drift move, so
# the reminder text is authored directly here.
GRADEABLE_MOVE_PREFIXES = ("anchor/", "coaching/", "escalate/")
GRADE_BACKSTOP_STALE_EVENTS = 40  # matches §2d's oscillation-span convention
GRADE_BACKSTOP_MOVE_ID = "mechanical/grade-backstop"

# Conservative imminent-action triggers (moves.md mechanical/announced-not-started).
# Matched against the LAST sentence of the last assistant text only. A
# trailing "?" always disqualifies first — a question to the user is never
# this signature, whatever it starts with. Future-conditional phrasing
# ("I'll do X once you confirm") never starts with any of these, so it's
# excluded by construction rather than by an extra negative check.
_STARTING_NOW_RE = re.compile(r"^(?:starting|doing)\b.*?\bnow\b", re.IGNORECASE | re.DOTALL)
_BEGINNING_RE = re.compile(r"^beginning\b", re.IGNORECASE)
_LET_ME_NOW_RE = re.compile(r"^let me now\b", re.IGNORECASE)


def _last_sentence(text):
    text = (text or "").strip()
    if not text:
        return ""
    parts = [p.strip() for p in re.split(r"(?<=[.!?])\s+|\n+", text) if p.strip()]
    return parts[-1] if parts else ""


def _is_announcement(sentence):
    if not sentence or sentence.endswith("?"):
        return False
    return bool(
        _STARTING_NOW_RE.match(sentence)
        or _BEGINNING_RE.match(sentence)
        or _LET_ME_NOW_RE.match(sentence)
    )


def _last_assistant_content(transcript_path):
    """One linear pass over the transcript, returning the LAST assistant
    message's content list (or None). Tolerant of malformed lines, matching
    observer.py's own transcript-reading style. This runs once per Stop
    event, not on a hot path, so a full scan is the simplest correct thing —
    no tail-seeking required."""
    last_content = None
    with open(transcript_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            if d.get("type") != "assistant":
                continue
            content = d.get("message", {}).get("content")
            if isinstance(content, list):
                last_content = content
    return last_content


def _announced_not_started(transcript_path):
    content = _last_assistant_content(transcript_path)
    if not content:
        return False
    last_text_idx, last_text, tool_use_after = None, None, False
    for i, block in enumerate(content):
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "text":
            last_text_idx, last_text = i, block.get("text", "")
        elif btype == "tool_use" and last_text_idx is not None and i > last_text_idx:
            tool_use_after = True
    if last_text is None or tool_use_after:
        return False
    return _is_announcement(_last_sentence(last_text))


def _sweep_stale_sentinels(verdicts_dir):
    try:
        now = time.time()
        for name in os.listdir(verdicts_dir):
            if ".stopblock." not in name:
                continue
            path = os.path.join(verdicts_dir, name)
            try:
                if now - os.path.getmtime(path) > STOPBLOCK_MAX_AGE:
                    os.remove(path)
            except OSError:
                pass
    except OSError:
        pass


def _session_gradeable_fires(telemetry_path, session_id):
    """(seq, move_id, ts) for every gradeable (anchor/coaching/escalate) fire
    delivered to THIS session's own mailbox (never a worker's — same
    session-only scope as §4b's scoring), oldest first. Reads telemetry.jsonl
    directly rather than needing a new field; never raises."""
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
                if rec.get("event") != "injected" or rec.get("agent_id"):
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


def _session_grade_count(eval_dir, session_id):
    """How many grade lines (any file matching live_grades*.jsonl) this
    session has already written — a coarse backstop count, not the precise
    per-fire join slice_fires.py does for the sleep pass."""
    import glob

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
                    if rec.get("session_id") == session_id:
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


def _grade_backstop_reason(ungraded_count, oldest_events_ago):
    return (
        f'<daemon move="{GRADE_BACKSTOP_MOVE_ID}">\n'
        f"This session delivered {ungraded_count} gradeable daemon fire(s) "
        f"(anchor/coaching/escalate) with no self-grade recorded yet, and the "
        f"oldest is {oldest_events_ago} tool events old. Before the session "
        f"ends, append one self-grade line per ungraded fire to "
        f".claude/daemon/eval/live_grades.session.jsonl — canonical "
        f"correct/effective values and format in RUNBOOK.md step 2, and "
        f'include each fire\'s own "seq" so the sleep pass can join the grade '
        f"back to the exact fire it belongs to.\n"
        f"</daemon>"
    )


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        if not session_id:
            return

        _sweep_stale_sentinels(valve.VERDICTS_DIR)

        # A turn is one prompt_id regardless of which mailbox (session-level
        # or a worker's) ends up supplying the whisper — the sentinel is
        # keyed on session+prompt, not on the agent-routed mailbox key.
        prompt_id = data.get("prompt_id") or ""
        sentinel_path = os.path.join(valve.VERDICTS_DIR, f"{session_id}.stopblock.{prompt_id}")
        if os.path.exists(sentinel_path):
            return  # this turn already spent its one block

        if data.get("stop_hook_active"):
            return  # possibly-undocumented re-entrancy guard; honor it if present

        agent_id = data.get("agent_id")
        # Same gate as daemon-posttooluse.py: agent-tagged Stop events are
        # fully dark until DESIGN.md §2b's worker-nudges flag is set.
        if agent_id and not valve.worker_nudges_enabled():
            return
        mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id

        def _block(reason, telemetry_extra):
            try:
                os.makedirs(valve.VERDICTS_DIR, exist_ok=True)
                open(sentinel_path, "w").close()
            except OSError:
                pass
            valve.append_telemetry(
                {
                    "ts": time.time(),
                    "session_id": session_id,
                    "agent_id": agent_id,
                    "event": "injected",
                    "valve": "Stop",
                    **telemetry_extra,
                }
            )
            print(json.dumps({"decision": "block", "reason": reason}))

        # 1. An already-pending, undelivered flag — never wait, never
        # classify synchronously; this only drains what the observer already
        # decided (DESIGN.md §2, RULED).
        block, seq, move_id = valve.pending_injection(mailbox_key)
        if block:
            valve.write_consumed(mailbox_key, seq)
            _block(block, {"seq": seq, "move_id": move_id})
            return

        # 1b. Catch-up wait (re-ruled 2026-07-05, supersedes the sleep-pass-1
        # classifying-marker wait): at Stop the observer has almost never
        # read the turn-final text yet — it polls every POLL_SECONDS, Stop
        # fires milliseconds after the text lands — so the old marker-only
        # wait missed the common case. Wait for the observer's `.offset`
        # heartbeat to reach the transcript size recorded at Stop entry;
        # classification is synchronous inside the observer's drain and the
        # heartbeat is written after the drain returns, so caught-up means
        # every verdict this turn can produce is already on disk. Skips
        # (fails open) when the observer is dead, the heartbeat file doesn't
        # exist, or the transcript is unreadable. Main session only —
        # workers' transcripts aren't heartbeat-tracked.
        STOP_WAIT_CAP_S = 6.0
        transcript_path = data.get("transcript_path")
        if not agent_id and transcript_path:
            try:
                target = os.path.getsize(transcript_path)
            except OSError:
                target = None
            offset_path = os.path.join(valve.VERDICTS_DIR, f"{session_id}.offset")
            if target is not None and os.path.exists(offset_path) and valve.observer_alive(session_id):
                deadline = time.time() + STOP_WAIT_CAP_S
                while True:
                    # Verdicts land inside the drain, before the heartbeat
                    # moves — check the mailbox first so a whisper delivers
                    # the moment it exists, not a poll later.
                    block, seq, move_id = valve.pending_injection(mailbox_key)
                    if block:
                        valve.write_consumed(mailbox_key, seq)
                        _block(block, {"seq": seq, "move_id": move_id, "stop_wait": True})
                        return
                    try:
                        with open(offset_path, encoding="utf-8") as f:
                            drained = int(f.read().strip() or "0")
                    except (OSError, ValueError):
                        break
                    if drained >= target or time.time() >= deadline:
                        break
                    time.sleep(0.25)
                # Caught up (or capped): one final mailbox read for a verdict
                # written by the drain that closed the gap.
                block, seq, move_id = valve.pending_injection(mailbox_key)
                if block:
                    valve.write_consumed(mailbox_key, seq)
                    _block(block, {"seq": seq, "move_id": move_id, "stop_wait": True})
                    return

        # 2. No pending flag: deterministic, valve-selected mechanical check.
        if not transcript_path or not os.path.exists(transcript_path):
            return
        if _announced_not_started(transcript_path):
            reason = valve.build_block({"move_id": "mechanical/announced-not-started"})
            if not reason:
                return
            _block(reason, {"move_id": "mechanical/announced-not-started"})
            return

        # 3. Grade-backstop (2026-07-05 review). Main-session only, fires at
        # most once per session (its own sentinel below, distinct from the
        # per-turn stopblock).
        if agent_id:
            return
        backstop_sentinel = os.path.join(valve.VERDICTS_DIR, f"{session_id}.grade-backstop-fired")
        if os.path.exists(backstop_sentinel):
            return
        fires = _session_gradeable_fires(valve.TELEMETRY_PATH, session_id)
        if not fires:
            return
        graded = _session_grade_count(valve.EVAL_DIR, session_id)
        if len(fires) <= graded:
            return
        events_ago = _events_since(transcript_path, fires[0][2])
        if events_ago <= GRADE_BACKSTOP_STALE_EVENTS:
            return
        try:
            os.makedirs(valve.VERDICTS_DIR, exist_ok=True)
            open(backstop_sentinel, "w").close()
        except OSError:
            pass
        _block(
            _grade_backstop_reason(len(fires) - graded, events_ago),
            {
                "move_id": GRADE_BACKSTOP_MOVE_ID,
                "ungraded_count": len(fires) - graded,
                "oldest_events_ago": events_ago,
            },
        )
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
