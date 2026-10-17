#!/usr/bin/env python3
"""Stop hook: a lead-session turn that ran tools or wrote a long reply must end
with mcp__ccd_turn__wrap_up (the Claude desktop app's summary box).

Two events, one file:
- PostToolUse on mcp__ccd_turn__wrap_up writes a marker keyed by session and
  prompt. Stop can fire before the wrap_up call reaches the transcript on disk,
  so the marker is the reliable proof the box was shown.
- Stop blocks with a reminder when neither the marker nor the transcript shows
  the call. It blocks up to MAX_BLOCKS times per prompt, then lets go so a
  broken box can never wedge a session.

Lead-session signal: the desktop app lists wrap_up in
CLAUDE_CODE_TERMINAL_MCP_TOOLS; the transcript naming it is the fallback.
Plain CLI sessions skip. Subagents fire SubagentStop, not Stop, so lanes never
reach this hook. Fails open on any error.
"""
import json
import os
import sys
import tempfile
from pathlib import Path

WRAP_UP = "mcp__ccd_turn__wrap_up"
LONG_REPLY_LINES = 7
MAX_BLOCKS = 2
MARKER_DIR = Path(os.environ.get("WRAP_UP_MARKER_DIR") or Path(tempfile.gettempdir()) / "manifold-wrap-up")
REASON = (
    "End this turn by calling mcp__ccd_turn__wrap_up now: a recap of what "
    "changed, what it means for the show, and what is not verified. Add ask "
    "plus actions only if a decision is pending; button labels 1-3 words, no "
    "message field."
)


def blocks(entry):
    content = (entry.get("message") or {}).get("content")
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    return [b for b in content if isinstance(b, dict)] if isinstance(content, list) else []


def is_real_user(entry):
    if entry.get("type") != "user" or entry.get("isMeta") or entry.get("isSidechain"):
        return False
    bs = blocks(entry)
    if not bs or any(b.get("type") == "tool_result" for b in bs):
        return False
    return any(b.get("type") in ("text", "image") for b in bs)


def current_turn(entries):
    last = -1
    for i, e in enumerate(entries):
        if is_real_user(e):
            last = i
    return entries[last + 1:]


def needs_wrap_up(turn, last_message=""):
    tools, final_text = [], ""
    for e in turn:
        if e.get("type") != "assistant":
            continue
        for b in blocks(e):
            if b.get("type") == "tool_use":
                tools.append(b.get("name", ""))
            elif b.get("type") == "text" and b.get("text", "").strip():
                final_text = b["text"]
    if WRAP_UP in tools:
        return False
    if any(t != "ToolSearch" for t in tools):
        return True
    final_text = last_message or final_text
    return len(final_text.strip().splitlines()) > LONG_REPLY_LINES


def marker_path(session_id):
    return MARKER_DIR / f"{session_id}.json"


def read_marker(session_id):
    try:
        return json.loads(marker_path(session_id).read_text())
    except (OSError, ValueError):
        return {}


def write_marker(session_id, data):
    MARKER_DIR.mkdir(parents=True, exist_ok=True)
    marker_path(session_id).write_text(json.dumps(data))


def is_lead(raw_text):
    return WRAP_UP in os.environ.get("CLAUDE_CODE_TERMINAL_MCP_TOOLS", "") or WRAP_UP in raw_text


def on_post_tool_use(payload):
    sid, pid = payload.get("session_id"), payload.get("prompt_id")
    if payload.get("tool_name") == WRAP_UP and sid and pid:
        write_marker(sid, {"prompt_id": pid, "wrapped": True})


def on_stop(payload):
    sid, pid = payload.get("session_id"), payload.get("prompt_id")
    marker = read_marker(sid) if sid and pid else {}
    if marker.get("prompt_id") != pid:
        marker = {}
    if marker.get("wrapped"):
        return None
    path = payload.get("transcript_path")
    if not path:
        return None
    with open(path, encoding="utf-8") as f:
        raw = f.read()
    if not is_lead(raw):
        return None
    entries = []
    for line in raw.splitlines():
        try:
            entries.append(json.loads(line))
        except ValueError:
            pass
    if not needs_wrap_up(current_turn(entries), payload.get("last_assistant_message") or ""):
        return None
    if sid and pid:
        count = marker.get("blocks", 0)
        if count >= MAX_BLOCKS:
            return None
        write_marker(sid, {"prompt_id": pid, "blocks": count + 1})
    elif payload.get("stop_hook_active"):
        return None
    return {"decision": "block", "reason": REASON}


def main():
    try:
        payload = json.load(sys.stdin)
        if payload.get("hook_event_name") == "PostToolUse":
            on_post_tool_use(payload)
            return
        out = on_stop(payload)
        if out:
            print(json.dumps(out))
    except Exception:
        return


if __name__ == "__main__":
    main()
