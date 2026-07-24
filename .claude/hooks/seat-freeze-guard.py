#!/usr/bin/env python3
"""PreToolUse hook: seat freeze — a stand-down agents cannot ignore.

Why (2026-07-24, Peter + lead, D-53): a GLM-5.2 dispatcher acknowledged a
stand-down message but continued working through its current turn — spawning
another lane after being told to stop — because mailbox messages are only
seen BETWEEN turns. Message-based stand-downs have turn latency; a frozen
seat must be PHYSICALLY unable to write, spawn, or run commands. Same
philosophy as the tier guards: enforced by machinery, not hoped-for behavior.

Mechanism (deterministic, no model calls): the lead writes
`.claude/orchestration/frozen-seats.json`:

    {"frozen": ["glm-5.2"], "reason": "quota rotation D-51", "set_by": "...", "set_at": "..."}

The hook identifies the caller's model from its transcript's last
`message.model` (same method as agent-tier-spawn-guard.py) and DENIES all
non-read-only tools (Bash/Edit/Write/MultiEdit/Agent/NotebookEdit) when the
caller model exactly matches a frozen entry. Read-only tools (Read/Grep/
Glob/LSP) pass, so a frozen seat can still answer a final state question.
Matching is EXACT model-string equality ("glm-5.2" does not freeze
"glm-4.7"); freeze every model string a seat can appear as if in doubt.

This does NOT replace TaskStop for halting a running seat — TaskStop kills
the process; the freeze guard covers the resumable-after-stop hole (agent
names stay addressable via SendMessage) and makes the state durable for any
future session that meets the same seat.

Fails open on any error (missing/unreadable freeze file or transcript,
format drift): a guard hook must never be able to block a session.
"""
import json
import os
import sys

TAIL_BYTES = 256 * 1024
DENY_TOOLS = {"Bash", "Edit", "Write", "MultiEdit", "NotebookEdit", "Agent"}


def caller_model(transcript_path: str) -> str:
    with open(transcript_path, "rb") as f:
        try:
            f.seek(-TAIL_BYTES, os.SEEK_END)
        except OSError:
            f.seek(0)
        tail = f.read().decode("utf-8", errors="replace")
    model = ""
    for line in tail.splitlines():
        if '"model"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        m = (entry.get("message") or {}).get("model") or entry.get("model") or ""
        if isinstance(m, str) and m:
            model = m
    return model


def load_frozen(project_dir: str):
    path = os.path.join(project_dir, ".claude", "orchestration", "frozen-seats.json")
    try:
        with open(path) as f:
            data = json.load(f)
    except (OSError, ValueError):
        return [], ""
    frozen = data.get("frozen") or []
    return [m for m in frozen if isinstance(m, str) and m], str(data.get("reason") or "")


def deny(reason: str) -> None:
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            }
        )
    )


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool = payload.get("tool_name") or ""
        if tool not in DENY_TOOLS:
            sys.exit(0)
        project_dir = os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
        frozen, freeze_reason = load_frozen(project_dir)
        if not frozen:
            sys.exit(0)
        transcript_path = payload.get("transcript_path") or ""
        if not transcript_path or not os.path.isfile(transcript_path):
            sys.exit(0)  # fail open — can't identify the caller
        model = caller_model(transcript_path)
        if model and model in frozen:
            deny(
                f"SEAT FROZEN by the lead ({freeze_reason or 'no reason recorded'}): "
                f"this session runs {model}, which is on the frozen list in "
                ".claude/orchestration/frozen-seats.json. Do not work. "
                "Read-only tools remain available if the lead asks you a "
                "final state question; anything else waits for the freeze to lift."
            )
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard hook must never block a session


if __name__ == "__main__":
    main()
