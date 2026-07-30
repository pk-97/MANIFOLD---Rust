#!/usr/bin/env python3
"""PreToolUse hook for Agent: enforce the spawn hierarchy by caller tier.

Why (2026-07-21, Peter + AGENT_ROUTING.md): executors deferring their own
work to sub-agents recreates executor-over-executor — the exact failure the
steering model exists to prevent — and until now it was policy, not
machinery. The chain is lead → (optional dispatcher) → executors, full stop.

Mechanism (deterministic, no model calls): the hook payload carries
`transcript_path` — the caller's own conversation JSONL. The last assistant
entry's `message.model` identifies the calling agent's tier.

Tier rules (D-48 native-lane roster, 2026-07-24 — lanes are native Agent
subagents whose slot env maps to provider models via the litellm proxy;
see docs/AGENT_ROUTING.md §Native provider lanes):

- LEAD (fable / claude-opus / k3): spawns anything.
- DISPATCHER / middle (glm-*): may spawn ONLY `model: "haiku"` — the
  DeepSeek Flash executor slot. Anything else (sonnet/opus/fable lanes,
  missing model) is denied: dispatchers drive executors, never peers or
  tiers above themselves.
- EXECUTOR (deepseek*, kimi-k2*, kimi-for-coding, claude-sonnet/haiku):
  ALL Agent spawns denied. Executors execute; decisions flow up.

Fails open on any error (missing/unreadable transcript, format drift): a
guard hook must never be able to block a session. `agent-launch-guard.py`
independently covers the explicit-model requirement for allowed spawns.

History: 2026-07-24 R2 extended DENY to the open provider roster (model ids
measured from real transcripts: `deepseek-v4-flash`, `glm-4.7`).
2026-07-24 D-48 split GLM out of the executor tier: subagent nesting is
harness-possible and the GLM dispatcher legitimately spawns haiku lanes.
2026-07-28: seat markers (`agent_id`/`agent_type`/`teammate_name` in the
payload) now deny BEFORE the transcript read — teammate payloads carry the
PARENT transcript, so the model check saw the lead's model and let a haiku
teammate spawn an Explore agent. The transcript-tier path below survives
only for the marker-less lead session.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the two-tier lead/lane model this guard polices; recheck at each routing-policy revision.
"""
import json
import os
import re
import sys

# glm-4.7 is dispatch-tier (Peter 2026-07-27): classifier-dedicated on the
# sonnet slot, but still a legitimate dispatcher — haiku-only spawns.
EXECUTOR_TIERS = re.compile(
    r"claude-(sonnet|haiku)|deepseek|kimi-k2|kimi-for-coding",
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

        # Seat markers first (2026-07-28, measured via telemetry `keys`):
        # subagent/teammate PreToolUse payloads carry `agent_id`/`agent_type`;
        # the lead's carry neither. Transcript-model detection CANNOT see
        # this — teammate payloads carry the PARENT transcript, so the model
        # reads as the lead and a haiku lane spawned an Explore agent
        # (2026-07-28). Marker present → deny: with the dispatcher seat
        # retired (CLAUDE.md Agents), no worker seat spawns anything; if a
        # dispatcher tier returns, reintroduce its allowance HERE, on
        # markers, never on transcript model.
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