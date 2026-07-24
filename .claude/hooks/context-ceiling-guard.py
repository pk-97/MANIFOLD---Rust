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

TIER SPLIT (Peter's ruling, 2026-07-24): the ceiling applies to workers and
dispatchers ONLY. The lead seat (model matches fable|k3 -- NOT opus; same
transcript-model detection as cc-fleet-tier-guard.py) is fully exempt: no
warn, no deny; auto-compaction is the backstop. For every other seat,
including unidentifiable ones, a ceiling hit is a defect signal and behavior
is unchanged.

Denial is not data loss: past DENY_TOKENS a WRAP-UP LANE stays open -- git
commands (commit clean work), Write/Edit to scratchpad / .claude/orchestration
/ handoff files, and SendMessage (report up). Everything else is denied. The
seat exits cleanly under its own power instead of stranding mid-task and
forcing the lead to spend tokens re-spinning it (Peter's concern, 2026-07-24).
A successor resumes from the handoff, which is the rotation protocol
AGENT_ROUTING.md already specifies.

Fails open on any error (missing/unreadable transcript, format drift): a guard
hook must never be able to block a session. Escape hatch for a genuinely
unavoidable long seat: MANIFOLD_CONTEXT_CEILING=off in the environment.
"""
import json
import os
import re
import sys

WARN_TOKENS = 150_000
DENY_TOKENS = 200_000
TAIL_BYTES = 512 * 1024

# Lead tier is exempt (Peter 2026-07-24): rotation ceilings are for workers.
# Lead = fable/k3 ONLY (Peter: Opus is not lead tier).
LEAD_TIER = re.compile(r"fable|\bk3\b", re.IGNORECASE)

# Wrap-up lane: tools still allowed past DENY_TOKENS so the seat can land
# clean work, write its handoff, and report up -- nothing else.
WRAPUP_PATH_MARKERS = ("/scratchpad", "/.claude/orchestration/", "handoff")


def is_wrapup_call(tool_name: str, tool_input: dict) -> bool:
    if tool_name == "SendMessage":
        return True
    if tool_name == "Bash":
        cmd = (tool_input.get("command") or "").strip()
        return cmd.startswith("git ")
    if tool_name in ("Write", "Edit"):
        path = (tool_input.get("file_path") or "").lower()
        return any(m in path for m in WRAPUP_PATH_MARKERS)
    return False


def current_context(transcript_path: str) -> tuple[int, str]:
    """(context size of the most recent assistant turn, caller model)."""
    with open(transcript_path, "rb") as f:
        try:
            f.seek(-TAIL_BYTES, os.SEEK_END)
        except OSError:
            f.seek(0)
        tail = f.read().decode("utf-8", errors="replace")
    size = 0
    model = ""
    for line in tail.splitlines():
        if '"usage"' not in line and '"model"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        message = entry.get("message") or {}
        m = message.get("model") or entry.get("model") or ""
        if isinstance(m, str) and m:
            model = m  # keep the LAST one seen
        usage = message.get("usage")
        if not isinstance(usage, dict):
            continue
        total = ((usage.get("cache_read_input_tokens") or 0)
                 + (usage.get("cache_creation_input_tokens") or 0)
                 + (usage.get("input_tokens") or 0))
        if total:
            size = total  # keep the LAST one seen
    return size, model


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

        size, model = current_context(path)
        if LEAD_TIER.search(model):
            sys.exit(0)  # lead seat is exempt -- workers only
        if size >= DENY_TOKENS:
            tool_name = payload.get("tool_name") or ""
            tool_input = payload.get("tool_input") or {}
            if is_wrapup_call(tool_name, tool_input if isinstance(tool_input, dict) else {}):
                emit("allow", (
                    f"Wrap-up lane (context ~{size//1000}K, past the {DENY_TOKENS//1000}K "
                    "ceiling): this call is allowed only to land clean work / write the "
                    "handoff / report up. Finish the rotation, then stop."
                ))
                sys.exit(0)
            emit("deny", (
                f"Context ceiling: this seat is at ~{size//1000}K tokens, past the "
                f"{DENY_TOKENS//1000}K hard limit. Every further call costs ~{size//1000}K "
                "cache-read for the same work (measured 41x penalty on long sessions -- "
                "docs/TOKEN_ECONOMICS.md §3c). STOP and rotate. Still allowed (wrap-up lane): "
                "git commands to commit clean work, Write/Edit of a handoff file in the "
                "scratchpad or .claude/orchestration/, SendMessage to report up. Write the "
                "handoff naming state/anchors/next step, then stop. Override only if "
                "genuinely unavoidable: MANIFOLD_CONTEXT_CEILING=off."
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
