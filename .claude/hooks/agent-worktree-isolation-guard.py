#!/usr/bin/env python3
"""PreToolUse hook for the Agent tool: deny `isolation: "worktree"`.

Two reasons, both standing (CLAUDE.md hard rule + the 2026-07-15 disk
incident): the built-in isolation bases its worktree off the default
branch rather than the session's tip, and it creates worktrees outside
the slot ring's structural cap (scripts/agent-worktree.py, max 6 slots).
Agents get worktrees from the ring, never from the harness.

Never blocks any other Agent call. Fail-safe: any parse failure emits
nothing (normal permission flow).
"""
import json
import sys


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0
    if data.get("tool_input", {}).get("isolation") != "worktree":
        return 0
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": (
                "Agent isolation: \"worktree\" is denied — it bases the "
                "worktree off the default branch (not your tip) and bypasses "
                "the slot ring's 6-slot cap (455 GB incident, 2026-07-15). "
                "Acquire a worktree via `python3 scripts/agent-worktree.py "
                "acquire <task-label> <branch> [--tip REF]` and point the "
                "agent at the printed slot path instead. Remote isolation "
                "and plain agents are unaffected."
            ),
        }
    }, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
