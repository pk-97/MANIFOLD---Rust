#!/usr/bin/env python3
"""PreToolUse hook for Agent: enforce teammate naming conventions.

Why (2026-07-25, Peter): "the hook needs to enforce all of the correct
naming and conventions for teammates such as their model name and role."
Soft rules lost: the lead spawned an invisible, unnamed-by-convention
background agent within a minute of its first lane task this session, and
opaque task labels (T1, D-52) were already a standing Peter rule
(memory: no-opaque-task-labels) with no machinery behind it.

The convention:
  name = "<slot>-<descriptive-task>"
  kebab-case; the task part must be descriptive — at least two alphabetic
  words; opaque label segments (T1, D52, W3, P2-G style) are denied.

The slot label is derived AT SPAWN TIME from the backend the harness will
actually use: the Agent tool's `model` param selects a tier slot, and the
session env (`ANTHROPIC_DEFAULT_<TIER>_MODEL`, written by cc-fleet into the
profile / injected by the tmux binding) says which backend model that slot
resolves to. No env var, or a `claude-*` value → Anthropic path, label =
the tier name itself. This replaces a hardcoded copy of the slot map that
seat_tool regex-edited into this file and that drifted on every rotation
(2026-07-27: docstring and deny text still said sonnet->flash after the
glm-4.7 repoint). SHORT_LABEL is the only human-maintained piece — extend
it when onboarding a model (seat_tool warns when it's missing).

A name that carries the slot makes every panel entry, inbox message, and
litellm ledger line self-describing: model AND role at a glance.

Behavior (deterministic, no model calls):
- subagent_type "fork" or a missing name -> allow (nameless spawns are the
  model guard's / harness's concern; this hook only judges names it can see).
- Wrong/missing slot prefix, bad casing, opaque task part -> deny with the
  fix and the live slot map spelled out.

Fails open on any error: a guard hook must never be able to block a session.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the slot-ring naming scheme this guard derives from; recheck at each routing-policy revision.
"""
import json
import os
import re
import sys

SLOT_ENV = {
    "haiku": "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "sonnet": "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "opus": "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "fable": "ANTHROPIC_DEFAULT_FABLE_MODEL",
}

# Lane-name labels per backend model. Fallback: last dash segment — ugly;
# extend when onboarding a model (seat_tool assign warns on a miss).
SHORT_LABEL = {
    "deepseek-v4-flash": "flash",
    "deepseek-v4-pro": "pro",
    "glm-4.7": "glm47",
    "glm-5.2": "glm52",
    "k3": "k3",
    "kimi-for-coding": "k27",
}

# Opaque label segments: T1, D52, D-52, W3, P2, P2G, R1, S8, BUG-NNN-style
# refs — fine INSIDE a descriptive name, denied as the whole task part.
OPAQUE_SEG = re.compile(r"^[a-z]?-?\d+[a-z]?$", re.IGNORECASE)


def backend_for_slot(model_param: str, env=os.environ) -> tuple[str, str]:
    """(backend model, slot label) for an Agent-tool `model` param.

    Env var unset or claude-* → Anthropic path: the tier name IS the label.
    """
    backend = (env.get(SLOT_ENV.get(model_param, ""), "") or "").strip()
    if not backend or backend.startswith("claude-"):
        return backend or model_param, model_param
    label = SHORT_LABEL.get(backend)
    if not label:
        label = re.sub(r"[^a-z0-9]", "", backend.split("-")[-1].lower()) or backend
    return backend, label


def slot_map(env=os.environ) -> dict:
    """{model_param: (backend, label)} for every tier slot, from live env."""
    return {p: backend_for_slot(p, env) for p in SLOT_ENV}


def describe_map(env=os.environ) -> str:
    return ", ".join(
        f"{p}->{label} ({backend})" for p, (backend, label) in slot_map(env).items()
    )


def decide(tool_input: dict, env=os.environ) -> str:
    """Deny reason, or "" to allow."""
    if (tool_input.get("subagent_type") or "").strip().lower() == "fork":
        return ""

    name = (tool_input.get("name") or "").strip()
    if not name:
        return ""  # nothing to judge; harness/hooks cover nameless spawns

    model = str(tool_input.get("model") or "").strip().lower()
    mapping = slot_map(env)
    valid_slots = sorted({label for _, label in mapping.values()})
    expected_backend, expected_slot = mapping.get(model, ("", ""))

    if name != name.lower() or not re.fullmatch(r"[a-z0-9-]+", name):
        return (
            f"Teammate name '{name}' violates the naming convention: "
            "kebab-case lowercase only. Format: <slot>-<descriptive-task>, "
            f"e.g. {valid_slots[0]}-beads-migration."
        )

    slot, _, task_part = name.partition("-")
    if slot not in valid_slots:
        return (
            f"Teammate name '{name}' must start with its model slot: one of "
            f"{', '.join(valid_slots)}-. This session's backend map (from slot "
            f"env): {describe_map(env)}. Rename, e.g. "
            f"'{expected_slot or valid_slots[0]}-{name}'."
        )

    if expected_slot and slot != expected_slot:
        return (
            f"Teammate name '{name}' claims slot '{slot}' but model=\"{model}\" "
            f"runs backend {expected_backend or model} in this session → slot "
            f"'{expected_slot}'. Name and model must agree: "
            f"'{expected_slot}-{task_part}'."
        )

    task_segs = [s for s in task_part.split("-") if s]
    alpha_words = [s for s in task_segs if re.search(r"[a-z]{3,}", s)]
    if not task_part or len(alpha_words) < 2 or all(
        OPAQUE_SEG.match(s) for s in task_segs
    ):
        return (
            f"Teammate name '{name}': task part '{task_part or '(empty)'}' is not "
            "descriptive. Name the WORK in plain words (>=2 words), never bare "
            "labels like T1/D-52 (Peter's no-opaque-task-labels rule). "
            f"e.g. '{slot}-migrate-bug-backlog'."
        )

    return ""


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        reason = decide(payload.get("tool_input") or {})
        if reason:
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
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open


if __name__ == "__main__":
    main()
