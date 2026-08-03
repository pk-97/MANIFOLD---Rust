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
- cc-fleet subagent/run/workflow: denied for EVERY tier (Peter 2026-08-02: "We use team lanes native — they provide a reliable messaging system"). Headless lanes are invisible, unmessageable, and outside the lane-health-check. Lanes spawn as native Agent-tool teammates via the slot map. `cc-fleet subagent-status` polls pass (not a spawn verb).

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
    # (docs/AGENT_ROUTING.md §Native provider lanes).
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
    # ALL remaining spawn verbs (subagent/run/workflow) denied for EVERY tier
    # (Peter 2026-08-02: "We use team lanes native — they provide a reliable
    # messaging system"). Headless cc-fleet lanes are invisible in the UI,
    # unreachable via SendMessage, and outside the lane-health-check cron;
    # the 2026-08-02 KEY_INVALID incident proved their failures hide.
    # `cc-fleet subagent-status` polls are not spawn verbs — SPAWN_CMD's
    # negative lookahead already excludes them.
    return (
        f"cc-fleet {verb} denied for every tier: lanes are NATIVE Agent-tool "
        "teammates only (Peter 2026-08-02) — headless cc-fleet spawns are "
        "invisible in the UI, unreachable via SendMessage, and outside the "
        "lane-health-check. Spawn a native lane instead: model \"haiku\" = "
        "DeepSeek Flash, \"sonnet\" = GLM-4.7, \"opus\" = GLM-5.2 "
        "(escalation seat), \"fable\" = k3, named with the slot prefix "
        "(flash-*/glm47-*/glm52-*/k3-*). `cc-fleet subagent-status <job>` "
        "polls of already-running jobs are unaffected."
    )


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
