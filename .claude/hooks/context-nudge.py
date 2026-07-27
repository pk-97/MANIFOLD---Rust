#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) — path-triggered invariant injection.

The semantic-instruction-set pattern applied to the harness: invariants are
DATA (.claude/context-nudges/table.json + snippet files), dispatch is
deterministic (glob match on the touched path), and each topic fires ONCE per
session. Rules arrive at the moment of contact instead of living in always-on
context or relying on model judgment to look them up.

Adding an invariant = edit a snippet or add a glob to the table. Never a new
hook. Fails open on any error.
"""
import fnmatch
import json
import os
import sys

BASE = os.path.join(os.path.dirname(__file__), "context-nudges")


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    if data.get("tool_name") not in ("Edit", "Write", "MultiEdit"):
        return 0
    file_path = (data.get("tool_input") or {}).get("file_path") or ""
    if not file_path:
        return 0

    try:
        table = json.load(open(os.path.join(BASE, "table.json")))["nudges"]
    except Exception:
        return 0

    session = data.get("session_id") or "nosession"
    state_path = f"/tmp/context_nudge_{session}.json"
    try:
        state = json.load(open(state_path))  # topic -> matching calls since last fire
    except Exception:
        state = {}

    # Re-inject after this many matching edits — long sessions push early
    # injections out of the effective window.
    REFIRE_AFTER = 15

    parts = []
    for entry in table:
        topic = entry.get("topic")
        if not any(fnmatch.fnmatch(file_path, g) for g in entry.get("globs", [])):
            continue
        if topic in state and state[topic] < REFIRE_AFTER:
            state[topic] += 1
            continue
        try:
            parts.append(open(os.path.join(BASE, entry["snippet"])).read().strip())
        except Exception:
            continue
        state[topic] = 0

    try:
        json.dump(state, open(state_path, "w"))
    except Exception:
        pass

    if not parts:
        return 0

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": (
                "<invariants topic-triggered by the file you are touching — "
                "apply them; injected once per session>\n"
                + "\n\n".join(parts) + "\n</invariants>"
            ),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
