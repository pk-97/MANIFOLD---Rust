#!/usr/bin/env python3
"""PreToolUse hook for Bash: tier-guard cc-fleet spawn commands.

Provider sessions (cc-fleet subagents/teammates) have no Agent tool, so
agent-tier-spawn-guard.py never sees their spawns — but they do have Bash, and the
sanctioned way to spawn a provider agent is a bash cc-fleet call. Without this guard,
Flash-over-Flash is one bash call away.

Caller tier comes from the payload's `transcript_path`: the last assistant entry's
`message.model`.

Tier rules (model strings: deepseek-v4-flash, glm-4.7, k3, claude-*):
- cc-fleet spawn (tmux teammates): denied for EVERY tier incl. lead — dead path on CC >= 2.1.218 (native Agent-tool lanes instead).
- ANY cc-fleet spawn verb carrying --background: denied for EVERY tier — a raw background spawn has no liveness exit code (Peter 2026-08-02); scripts/fleet_spawn.py owns background spawns and exits nonzero on a dead-at-spawn job.
- Executor tier (deepseek*, kimi-k2*, kimi-for-coding, claude-sonnet/haiku): ALL cc-fleet spawn verbs denied.
- Dispatcher tier (glm*): may drive the executor provider only (EXECUTOR_PROVIDERS = opencode, deepseek) via cc-fleet subagent. Anything else — spawning zai/kimi seats, workflows, unparseable targets — is denied with an escalate-up pointer.
- Lead tier (fable/opus/k3 — anything not matched above): passes through.

Fails open on any error — a guard hook must never block a session.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the provider-tier
model this guard polices, or cc-fleet is removed from the toolchain.
"""
import json
import os
import re
import sys

# Command-position match only: `cc-fleet` at the start of the command or
# right after a shell separator (&&, ||, ;, |, $(, backtick, newline),
# optionally behind env-var assignments. A quoted mention — an rg pattern,
# a commit message — is prose, not an invocation (two real false positives
# on 2026-07-24: a pathspec commit and a read-only rg sweep).
SPAWN_CMD = re.compile(
    r"(?:^|&&|\|\||;|\||\$\(|`|\n)\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S+\s+)*"
    r"(?:\S*/)?cc-fleet\s+(subagent|spawn|run|workflow)(?![\w-])(?:\s+(\S+))?"
)
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
    # `cc-fleet spawn` (tmux teammates) is a DEAD PATH on Claude Code >= 2.1.218
    # — TeamCreate is retired, teams are implicit, and the harness cannot
    # address externally-registered teammates. Denied for EVERY tier, lead
    # included. Provider lanes are native Agent-tool subagents via the slot map
    # (docs/AGENT_ROUTING.md §Native provider lanes). `cc-fleet subagent`
    # one-shots remain available per tier below.
    if verb == "spawn":
        return (
            "cc-fleet spawn denied for every tier: the tmux-teammate path is "
            "dead on this harness (TeamCreate retired; teammates unreachable "
            "via SendMessage). Spawn provider lanes as native Agent-tool "
            "subagents instead: "
            "model \"haiku\"=DeepSeek Flash, \"sonnet\"=GLM-4.7, "
            "\"opus\"=GLM-5.2, \"fable\"=k3 on the K3 seat "
            "(docs/AGENT_ROUTING.md §Native provider lanes)."
        )
    # Raw --background spawns are denied for EVERY tier (Peter 2026-08-02:
    # "exit codes are required" — a background spawn exits 0 with
    # {"ok": true} even when the provider key is dead; the job fails minutes
    # later and only a manual poll sees it). scripts/fleet_spawn.py owns the
    # spawn + a liveness grace window and exits nonzero on a dead-at-spawn
    # job. A subagent-status poll is not a spawn — SPAWN_CMD already limits
    # this to spawn verbs.
    if "--background" in command:
        return (
            f"cc-fleet {verb} --background denied for every tier: a raw "
            "background spawn has no liveness exit code (2026-08-02: a dead "
            "provider key returned ok:true and the job failed 3 minutes "
            "later, invisible until a manual poll). Use the wrapper: "
            "scripts/fleet_spawn.py <provider> --model <id> --timeout 90m "
            "--max-budget-usd 5 --prompt '<brief>' — it exits nonzero when "
            "the job dies inside the liveness grace window. Synchronous "
            "subagent runs (no --background) are unaffected."
        )
    if not model:
        return ""
    if EXECUTOR_TIER.search(model):
        return (
            f"cc-fleet {verb} denied: this session runs {model} — an executor "
            "tier. Executors execute; they never spawn agents at any depth "
            "(docs/AGENT_ROUTING.md). STOP and report the need up to the "
            "lead instead."
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
