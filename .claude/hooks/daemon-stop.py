#!/usr/bin/env python3
"""Stop hook: the daemon's turn-end valve (DESIGN.md §2 "Known delivery gap",
RULED 2026-07-04, spec confirmed against the current hooks reference).

A flag raised on a turn's FINAL assistant text has no delivery channel until
the next human prompt — PostToolUse never fires again that turn, and that is
exactly where verify-claim's most common firing position (done-claims are
turn-final) lands one turn late. This hook may block the Stop event ONCE,
delivering that already-pending whisper as the block reason so the model
gets one beat to self-correct before yielding. It never waits for
classification and never classifies synchronously — the race between the
observer's ~5-10s verdict latency and Stop firing immediately is accepted;
a turn-final flag with no verdict yet stays delivered next-prompt, same as
before this hook existed.

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

        # 2. No pending flag: deterministic, valve-selected mechanical check.
        transcript_path = data.get("transcript_path")
        if not transcript_path or not os.path.exists(transcript_path):
            return
        if _announced_not_started(transcript_path):
            reason = valve.build_block({"move_id": "mechanical/announced-not-started"})
            if not reason:
                return
            _block(reason, {"move_id": "mechanical/announced-not-started"})
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
