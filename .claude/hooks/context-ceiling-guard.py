#!/usr/bin/env python3
"""PreToolUse hook: warn, then stop, when a seat's context passes the rotation ceiling.

Why (2026-07-23, measured — docs/TOKEN_ECONOMICS.md §3c): cost per call grows
with position in a session because every call re-reads the whole context.
Measured on 14 days of real transcripts:

    call 0-49    ->  76K avg context
    call 300-349 -> 321K
    call 550-599 -> 460K
    call 1100+   -> 749K

A session of 400+ calls averages 176 MTok; one under 100 calls averages 4 MTok
-- 41x for the same kind of work. 12% of sessions burn 50% of all tokens.

AGENT_ROUTING.md §Overnight previously set the handoff ceiling at "~500K observed
as the sensible ceiling". The measurement says that is ~2.5x too high: by 500K
every remaining call costs half a megatoken. This hook enforces ~200K instead,
which is where the curve is still cheap.

Mechanism (deterministic, no model calls): the payload carries `transcript_path`
-- the caller's own JSONL. Read the last assistant entry's usage block; that
IS the current context size (cache_read + cache_creation + input). Warn at
WARN_TOKENS, deny at DENY_TOKENS with instructions to hand off.

Denial is not data loss: the seat writes a handoff file and a successor resumes
from it, which is the rotation protocol AGENT_ROUTING.md already specifies.

Fails open on any error (missing/unreadable transcript, format drift): a guard
hook must never be able to block a session. Escape hatch for a genuinely
unavoidable long seat: MANIFOLD_CONTEXT_CEILING=off in the environment.
"""
import json
import os
import sys

WARN_TOKENS = 200_000
DENY_TOKENS = 320_000
TAIL_BYTES = 512 * 1024


def current_context(transcript_path: str) -> int:
    """Context size of the most recent assistant turn, in tokens."""
    with open(transcript_path, "rb") as f:
        try:
            f.seek(-TAIL_BYTES, os.SEEK_END)
        except OSError:
            f.seek(0)
        tail = f.read().decode("utf-8", errors="replace")
    size = 0
    for line in tail.splitlines():
        if '"usage"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        usage = (entry.get("message") or {}).get("usage")
        if not isinstance(usage, dict):
            continue
        total = ((usage.get("cache_read_input_tokens") or 0)
                 + (usage.get("cache_creation_input_tokens") or 0)
                 + (usage.get("input_tokens") or 0))
        if total:
            size = total  # keep the LAST one seen
    return size


def emit(decision: str, reason: str) -> None:
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    }))


def main() -> None:
    try:
        if os.environ.get("MANIFOLD_CONTEXT_CEILING", "").lower() == "off":
            sys.exit(0)

        payload = json.load(sys.stdin)
        path = payload.get("transcript_path") or ""
        if not path or not os.path.isfile(path):
            sys.exit(0)  # fail open -- can't measure

        size = current_context(path)
        if size >= DENY_TOKENS:
            emit("deny", (
                f"Context ceiling: this seat is at ~{size//1000}K tokens, past the "
                f"{DENY_TOKENS//1000}K hard limit. Every further call costs ~{size//1000}K "
                "cache-read for the same work (measured 41x penalty on long sessions -- "
                "docs/TOKEN_ECONOMICS.md §3c). STOP and rotate: commit clean work, write a "
                "handoff file naming state/anchors/next step, and report that the seat needs "
                "a successor. Do not continue in this session. Override only if genuinely "
                "unavoidable: MANIFOLD_CONTEXT_CEILING=off."
            ))
        elif size >= WARN_TOKENS:
            emit("allow", (
                f"Context at ~{size//1000}K (ceiling {DENY_TOKENS//1000}K). Head for a clean "
                "commit boundary and hand off -- cost per call scales with context, so the "
                "tail of a long session is the most expensive work you do "
                "(docs/TOKEN_ECONOMICS.md §3c)."
            ))
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open -- a guard hook must never block a session


if __name__ == "__main__":
    main()
