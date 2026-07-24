#!/usr/bin/env python3
"""PreToolUse hook for Bash: tier-guard `cc-fleet` spawn commands.

Why (2026-07-24, R2 resolution — AGENT_ROUTING.md §0): provider sessions
(cc-fleet subagents/teammates) have no Agent tool, so
agent-tier-spawn-guard.py never sees their spawns — but they DO have Bash,
and the sanctioned way to spawn a provider agent IS a bash `cc-fleet` call.
Without this guard, Flash-over-Flash is one bash call away — the
executor-over-executor failure that killed the overnight waves.

Tier rules (model strings measured from real transcripts 2026-07-24:
`deepseek-v4-flash`, `glm-4.7`, `k3`, `claude-*`):

- `cc-fleet spawn` (tmux teammates): denied for EVERY tier incl. lead —
  dead path on CC >= 2.1.218 (D-48; native Agent-tool lanes instead).
- Executor tier (deepseek*, kimi-k2*, kimi-for-coding, claude-sonnet/haiku):
  ALL cc-fleet spawn verbs denied. Executors execute; decisions flow up.
- Dispatcher tier (glm*): may drive the executor provider only
  (EXECUTOR_PROVIDERS) via `cc-fleet subagent`. Anything else — spawning
  glm/kimi seats, workflows, unparseable targets — is denied with an
  escalate-up pointer.
- Lead tier (fable/opus/k3 — anything not matched above): passes through.

Fails open on any error — a guard hook must never block a session.
"""
import json
import os
import re
import sys

SPAWN_CMD = re.compile(r"\bcc-fleet\s+(subagent|spawn|run|workflow)(?![\w-])(?:\s+(\S+))?")
EXECUTOR_TIER = re.compile(
    r"claude-(sonnet|haiku)|deepseek|kimi-k2|kimi-for-coding", re.IGNORECASE
)
DISPATCHER_TIER = re.compile(r"\bglm", re.IGNORECASE)
# Providers a dispatcher may drive (the mechanical-executor seat).
EXECUTOR_PROVIDERS = {"opencode", "deepseek"}
TAIL_BYTES = 256 * 1024


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


def decide(command: str, model: str) -> str:
    """Return a deny reason, or '' to allow."""
    m = SPAWN_CMD.search(command)
    if not m:
        return ""
    verb, target = m.group(1), (m.group(2) or "")
    # D-48 (2026-07-24): `cc-fleet spawn` (tmux teammates) is a DEAD PATH on
    # Claude Code >= 2.1.218 — TeamCreate is retired, teams are implicit, and
    # the harness cannot address externally-registered teammates. Denied for
    # EVERY tier, lead included. Provider lanes are native Agent-tool
    # subagents via the slot map (docs/AGENT_ROUTING.md §Native provider
    # lanes). `cc-fleet subagent` one-shots remain available per tier below.
    if verb == "spawn":
        return (
            "cc-fleet spawn denied for every tier: the tmux-teammate path is "
            "dead on this harness (TeamCreate retired; teammates unreachable "
            "via SendMessage — D-48, .claude/orchestration/decisions.md). "
            "Spawn provider lanes as native Agent-tool subagents instead: "
            "model \"haiku\"=DeepSeek Flash, \"sonnet\"=GLM-4.7, "
            "\"opus\"=GLM-5.2, \"fable\"=k3 on the K3 seat "
            "(docs/AGENT_ROUTING.md §Native provider lanes)."
        )
    if not model:
        return ""
    if EXECUTOR_TIER.search(model):
        return (
            f"cc-fleet {verb} denied: this session runs {model} — an executor "
            "tier. Executors execute; they never spawn agents at any depth "
            "(docs/AGENT_ROUTING.md). STOP and report the need up to your "
            "dispatcher instead."
        )
    if DISPATCHER_TIER.search(model):
        if verb == "subagent" and target in EXECUTOR_PROVIDERS:
            return ""
        return (
            f"cc-fleet {verb} {target or ''} denied: this session runs {model} "
            "— the dispatcher tier, which may only drive the executor provider "
            f"({', '.join(sorted(EXECUTOR_PROVIDERS))}) via `cc-fleet subagent` "
            "(docs/AGENT_ROUTING.md §0 R6). Anything else escalates to the lead."
        )
    return ""


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        command = (payload.get("tool_input") or {}).get("command") or ""
        if "cc-fleet" not in command:
            sys.exit(0)
        transcript_path = payload.get("transcript_path") or ""
        if not transcript_path or not os.path.isfile(transcript_path):
            sys.exit(0)  # fail open — can't identify the caller
        reason = decide(command, caller_model(transcript_path))
        if reason:
            deny(reason)
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard hook must never block a session


if __name__ == "__main__":
    main()
