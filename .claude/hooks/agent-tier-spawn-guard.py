#!/usr/bin/env python3
"""PreToolUse hook for Agent: deny spawns from Sonnet/Haiku-tier callers.

Why (2026-07-21, Peter + AGENT_ROUTING.md): Sonnet lanes deferring their own
work to sub-agents recreates Sonnet-over-Sonnet — the exact failure the
steering model exists to prevent — and until now it was policy, not
machinery. The orchestration chain is Fable → (optional Opus orchestrator)
→ Sonnet executors, full stop. Executors execute.

Mechanism (deterministic, no model calls): the hook payload carries
`transcript_path` — the caller's own conversation JSONL. The last assistant
entry's `message.model` identifies the calling agent's tier. If it is a
sonnet/haiku model, deny the Agent spawn with a pointer to the routing doc.
Judgment tiers (fable/opus/kimi) pass through untouched.

Fails open on any error (missing/unreadable transcript, format drift): a
guard hook must never be able to block a session. `agent-model-guard.py`
independently covers the explicit-model requirement for allowed spawns.

2026-07-24 (R2, AGENT_ROUTING.md §0): extended to the open roster. Provider
sessions carry their provider model id in `message.model` (measured:
`deepseek-v4-flash`, `glm-4.7`) — executor tier (deepseek*, kimi-k2*,
kimi-for-coding) and the GLM dispatcher tier are denied Agent spawns: the
dispatcher drives executors via `cc-fleet` bash calls (guarded by
cc-fleet-tier-guard.py), never the Agent tool. Lead tiers (fable/opus/k3)
pass through.
"""
import json
import os
import re
import sys

DENY_TIERS = re.compile(
    r"claude-(sonnet|haiku)|deepseek|\bglm-|kimi-k2|kimi-for-coding", re.IGNORECASE
)
TAIL_BYTES = 256 * 1024  # models appear on every assistant entry; tail is plenty


def caller_model(transcript_path: str) -> str:
    with open(transcript_path, "rb") as f:
        try:
            f.seek(-TAIL_BYTES, os.SEEK_END)
        except OSError:
            f.seek(0)
        tail = f.read().decode("utf-8", errors="replace")
    model = ""
    for line in tail.splitlines():
        # Cheap pre-filter before json.loads.
        if '"model"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        m = (entry.get("message") or {}).get("model") or entry.get("model") or ""
        if isinstance(m, str) and m:
            model = m  # keep the LAST one seen
    return model


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        transcript_path = payload.get("transcript_path") or ""
        if not transcript_path or not os.path.isfile(transcript_path):
            sys.exit(0)  # fail open — can't identify the caller

        model = caller_model(transcript_path)
        if model and DENY_TIERS.search(model):
            print(
                json.dumps(
                    {
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": (
                                f"Agent spawn denied: this session runs {model} — an "
                                "executor tier. Executors execute; they never spawn "
                                "sub-agents (Sonnet-over-Sonnet, docs/AGENT_ROUTING.md). "
                                "If the task genuinely needs delegation, STOP and report "
                                "that up to your orchestrator instead."
                            ),
                        }
                    }
                )
            )
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard hook must never block a session


if __name__ == "__main__":
    main()
