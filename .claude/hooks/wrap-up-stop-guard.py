#!/usr/bin/env python3
"""Stop hook: a lead-session turn that ran tools or wrote a long reply must end
with mcp__ccd_turn__wrap_up (the Claude desktop app's summary box).

Blocks the stop once with a reminder. Fails open on any error.

Lead-session signal: the transcript text names mcp__ccd_turn__wrap_up (the
tool definition is recorded in the session when the desktop app offers it).
Plain CLI sessions and anything without the box skip. Subagents fire
SubagentStop, not Stop, so lanes never reach this hook.
"""
import json
import sys

WRAP_UP = "mcp__ccd_turn__wrap_up"
LONG_REPLY_LINES = 7
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


def needs_wrap_up(turn):
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
    return len(final_text.strip().splitlines()) > LONG_REPLY_LINES


def decide(entries, raw_text, stop_hook_active):
    if stop_hook_active or WRAP_UP not in raw_text:
        return None
    if needs_wrap_up(current_turn(entries)):
        return {"decision": "block", "reason": REASON}
    return None


def main():
    try:
        payload = json.load(sys.stdin)
        path = payload.get("transcript_path")
        if not path:
            return
        with open(path, encoding="utf-8") as f:
            raw = f.read()
        entries = []
        for line in raw.splitlines():
            try:
                entries.append(json.loads(line))
            except ValueError:
                pass
        out = decide(entries, raw, bool(payload.get("stop_hook_active")))
        if out:
            print(json.dumps(out))
    except Exception:
        return


if __name__ == "__main__":
    main()
