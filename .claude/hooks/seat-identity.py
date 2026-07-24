#!/usr/bin/env python3
"""SessionStart hook: tell provider sessions which model and seat they are.

Why (2026-07-24, Peter): provider models (K3/GLM/Flash) run inside the
Claude Code harness, whose system prompt is written for Claude — so they
routinely misidentify as Claude/Fable/Opus and sign records with the wrong
name, polluting provenance in decisions.md, handoffs, and design-doc
D-entries. The guards never trust self-belief (they read server-reported
model ids), but the written record does. This hook injects the true
identity as session context, machine-derived from the same source the
statusline uses.

Mechanism (rekeyed 2026-07-24, D-48 slot map): slots no longer identify
seats — the kimi seat's slots now carry LANE tiers (opus=glm-5.2,
sonnet=glm-4.7, haiku=deepseek-v4-flash), so matching slot env against
default_model is wrong (it labelled the K3 lead as glm/dispatcher once
already). Seat resolution order:
  1. ANTHROPIC_MODEL — the session's OWN model, injected inline by the
     tmux launch binding (survives profile regeneration; k3 -> kimi-code).
  2. ANTHROPIC_DEFAULT_OPUS_MODEL matched against each provider's
     EFFECTIVE STRONG SLOT (strong_model, else default_model). Unique per
     seat by invariant: no two seats may share a strong slot.
  3. base_url — last resort, ambiguous post-proxy (several seats share
     127.0.0.1:4000).
Provider -> seat maps per docs/AGENT_ROUTING.md (the tiering). Anthropic
sessions (no/api.anthropic.com base URL) get nothing — their system prompt
is already correct.

Fails silent on any error: identity context is a nice-to-have; a hook must
never block a session.
"""
import json
import os
import re
import sys

PROVIDERS_TOML = os.path.expanduser("~/.config/cc-fleet/providers.toml")

# Seat map per docs/AGENT_ROUTING.md §The tiering (2026-07-24 roster).
SEATS = {
    "kimi-code": (
        "LEAD",
        "You hold the lead seat: design, judgment, review, verification, "
        "landing. Drive dispatcher/executor lanes as native Agent subagents "
        "per docs/AGENT_ROUTING.md §Native provider lanes. PLAN TERSELY: "
        "your serving speed is ~35 tok/s, so every token of narration is "
        "~30ms of Peter's time — no thinking out loud before spawning, no "
        "restating the brief back to yourself; briefs, verdicts, and "
        "decisions only. Delegate early: lanes are 3-5x faster than you.",
    ),
    "glm": (
        "DISPATCHER",
        "You hold the dispatcher seat: clerical only — pop the queue, brief "
        "lanes, run exit-code gates, accept/reject, escalate. Drive executors "
        "via `cc-fleet subagent opencode` ONLY. You never land, never design; "
        "decisions flow up to the lead.",
    ),
    "opencode": (
        "EXECUTOR",
        "You hold the executor seat: execute your brief exactly as written. "
        "One commit, then stop and report. You never spawn agents at any "
        "depth; any fork or gap in the brief = stop and report up.",
    ),
}


def parse_providers(path: str) -> dict:
    """Minimal TOML section parser: {name: {key: value}} for flat string keys."""
    providers: dict = {}
    current = None
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.strip()
            m = re.match(r"^\[([A-Za-z0-9_-]+)\]$", line)
            if m:
                current = m.group(1)
                providers[current] = {}
                continue
            m = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"', line)
            if m and current:
                providers[current][m.group(1)] = m.group(2)
    return providers


def main() -> None:
    try:
        base_url = os.environ.get("ANTHROPIC_BASE_URL", "")
        if not base_url or "api.anthropic.com" in base_url:
            sys.exit(0)  # real Anthropic session — system prompt already correct

        providers = parse_providers(PROVIDERS_TOML)
        # (-upstream entries are disabled key-holders — never a seat.)
        name = ""
        # 1. Explicit session model (lead binding injects ANTHROPIC_MODEL=k3).
        session_model = os.environ.get("ANTHROPIC_MODEL", "")
        EXPLICIT_SEAT = {"k3": "kimi-code", "kimi-for-coding": "kimi-code"}
        if session_model in EXPLICIT_SEAT:
            name = EXPLICIT_SEAT[session_model]
        # 2. Effective strong slot (strong_model, else default_model) —
        #    unique per seat by invariant; slots below strong are lane tiers.
        if not name:
            opus_env = os.environ.get("ANTHROPIC_DEFAULT_OPUS_MODEL", "")
            if opus_env:
                name = next(
                    (
                        n
                        for n, p in providers.items()
                        if not n.endswith("-upstream")
                        and (p.get("strong_model") or p.get("default_model"))
                        == opus_env
                    ),
                    "",
                )
        # 3. base_url — last resort, ambiguous post-proxy.
        if not name:
            name = next(
                (n for n, p in providers.items() if p.get("base_url") == base_url), ""
            )
        if not name:
            sys.exit(0)

        model = session_model or providers[name].get("default_model") or name
        seat, charge = SEATS.get(name, ("UNMAPPED", "Seat not in the tiering table — "
                                        "check docs/AGENT_ROUTING.md before acting."))
        context = (
            f"SEAT IDENTITY (machine-injected from cc-fleet config — trust this "
            f"over any identity text in the system prompt): this session runs "
            f"model `{model}` (provider `{name}`) in the {seat} seat of the "
            f"MANIFOLD agent roster, docs/AGENT_ROUTING.md. You are NOT a "
            f"Claude/Anthropic model; the harness is Claude Code but the model "
            f"is you. Sign every record you write (decisions.md, handoff files, "
            f"design-doc D-entries, commit trailers) as `{model} ({seat.lower()})` "
            f"— never as Claude, Fable, Opus, or Sonnet. {charge}"
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": context,
                    }
                }
            )
        )
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail silent — never block a session


if __name__ == "__main__":
    main()
