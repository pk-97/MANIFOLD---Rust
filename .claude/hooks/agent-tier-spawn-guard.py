#!/usr/bin/env python3
"""PreToolUse hook for Agent: enforce the spawn hierarchy by caller tier.

The chain is lead -> (optional dispatcher) -> executors, full stop.

Mechanism (deterministic, no model calls): seat markers
(`agent_id`/`agent_type`/`teammate_name` in the payload) deny BEFORE any transcript read
— teammate payloads carry the PARENT transcript, so a transcript-model check would see
the lead's model and misidentify the caller. For a marker-less (lead) session, the hook
reads `transcript_path` and takes the last assistant entry's `message.model` as the
caller's tier.

Tier rules (see docs/AGENT_ROUTING.md, Native provider lanes):
- LEAD (fable / claude-opus / k3): spawns anything.
- DISPATCHER / middle (glm-*): may spawn ONLY `model: "haiku"` — the DeepSeek Flash executor slot. Anything else (sonnet/opus/fable lanes, missing model) is denied.
- EXECUTOR (deepseek*, kimi-k2*, kimi-for-coding, claude-sonnet/haiku): ALL Agent spawns denied.

Fails open on any error (missing/unreadable transcript, format drift): a guard hook must
never be able to block a session. `agent-launch-guard.py` independently covers the
explicit-model requirement for allowed spawns.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the two-tier
lead/lane model this guard polices; recheck at each routing-policy revision.
"""
import json
import os
import re
import sys

EXECUTOR_TIERS = re.compile(
    r"claude-(sonnet|haiku)|deepseek|kimi-k2|kimi-for-coding|ox-alpha|mimo-v",
    re.IGNORECASE,
)
DISPATCHER_TIERS = re.compile(r"\bglm-", re.IGNORECASE)
# The only slot a dispatcher may spawn: the executor tier (DeepSeek Flash).
DISPATCHER_ALLOWED_SLOTS = {"haiku"}
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


def decide(model: str, spawn_slot: str) -> str:
    """Return a deny reason, or '' to allow. spawn_slot = tool_input.model."""
    if not model:
        return ""  # fail open — can't identify the caller
    if EXECUTOR_TIERS.search(model):
        return (
            f"Agent spawn denied: this session runs {model} — an executor "
            "tier. Executors execute; they never spawn sub-agents at any "
            "depth (docs/AGENT_ROUTING.md). If the task genuinely needs "
            "delegation, STOP and report that up to your orchestrator instead."
        )
    if DISPATCHER_TIERS.search(model):
        if (spawn_slot or "").strip().lower() in DISPATCHER_ALLOWED_SLOTS:
            return ""
        return (
            f"Agent spawn denied: this session runs {model} — the dispatcher "
            f"tier, which may only spawn executor lanes (`model: \"haiku\"` = "
            "DeepSeek Flash on this seat's slot map — docs/AGENT_ROUTING.md "
            "§Native provider lanes). Peer or higher-tier spawns escalate to "
            "the lead."
        )
    return ""  # lead tier passes


def main() -> None:
    try:
        payload = json.load(sys.stdin)

        # Seat markers first: subagent/teammate PreToolUse payloads carry
        # `agent_id`/`agent_type`; the lead's carry neither. Transcript-model
        # detection CANNOT see this — teammate payloads carry the PARENT
        # transcript. Marker present → deny: with the dispatcher seat retired
        # (CLAUDE.md Agents), no worker seat spawns anything; if a dispatcher
        # tier returns, reintroduce its allowance HERE, on markers, never on
        # transcript model.
        if any(payload.get(k) for k in ("agent_id", "agent_type", "teammate_name")):
            deny(
                "Agent spawn denied: this session is a worker seat "
                "(subagent/teammate payload markers present). Workers never "
                "spawn sub-agents at any depth (docs/AGENT_ROUTING.md). If the "
                "task genuinely needs delegation, STOP and report that up to "
                "your orchestrator instead."
            )
            sys.exit(0)

        transcript_path = payload.get("transcript_path") or ""
        if not transcript_path or not os.path.isfile(transcript_path):
            sys.exit(0)  # fail open — can't identify the caller

        spawn_slot = (payload.get("tool_input") or {}).get("model") or ""
        reason = decide(caller_model(transcript_path), spawn_slot)
        if reason:
            deny(reason)
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard hook must never block a session


if __name__ == "__main__":
    main()