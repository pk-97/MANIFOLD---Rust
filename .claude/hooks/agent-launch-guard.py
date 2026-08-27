#!/usr/bin/env python3
"""PreToolUse hook for Agent: one-pass launch guard — model tier + teammate naming.

Checks everything in one pass; a deny spells the complete corrected call — model AND
exact name — so one retry always lands. Never auto-fills the model or rewrites the name:
passing the tier explicitly IS the sign-off.

Model rule:
- `model` absent -> deny. House default for workers is "sonnet"; "opus"/"fable" are an explicit per-launch decision.
- "opus"/"fable" -> allowed with a reminder attached.

Naming rule: name = "<slot>-<descriptive-task>", kebab-case, task part >= 2 plain words
(opaque labels like T1/D-52 denied: no-opaque-task-labels rule). The slot label is
derived AT SPAWN TIME from the backend the harness will actually use: the `model` param
selects a tier slot; the session env (`ANTHROPIC_DEFAULT_<TIER>_MODEL`) says which
backend that slot resolves to. Unset or claude-* -> Anthropic path, label = tier name.
SHORT_LABEL is the only human-maintained piece — extend it when onboarding a model
(seat_tool warns when it's missing).

seat_tool.py and gate_runner.py import backend_for_slot()/slot_map() from this file — it
is the single source of truth for slot labels.

Fails open on any error: a guard hook must never be able to block a session.

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the two-tier
lead/lane model or the slot-ring naming scheme; recheck at each routing-policy revision.
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
    "ox-alpha-free": "ox",
    "mimo-v2.5": "v25",
    "deepseek-v4-pro": "pro",
    "glm-4.7": "glm47",
    "glm-5.2": "glm52",
    "k3": "k3",
    "kimi-for-coding": "k27",
}

# Opaque label segments: T1, D52, D-52, W3, P2, P2G, R1, S8, BUG-NNN-style
# refs — fine INSIDE a descriptive name, denied as the whole task part.
OPAQUE_SEG = re.compile(r"^[a-z]?-?\d+[a-z]?$", re.IGNORECASE)

DEFAULT_MODEL = "sonnet"


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


def decide(tool_input: dict, env=os.environ) -> tuple[str, str]:
    """(deny_reason, allow_note). Deny reason lists EVERY defect and spells
    the one corrected call; allow_note carries the opus/fable reminder."""
    if (tool_input.get("subagent_type") or "").strip().lower() == "fork":
        return "", ""

    defects: list[str] = []
    note = ""

    model = tool_input.get("model")
    if model is None:
        defects.append(
            "no explicit `model` — it would silently inherit the "
            "orchestrator's tier and double-bill the worker (2026-07-06 "
            f'incident). "{DEFAULT_MODEL}" is the house worker default; '
            '"opus"/"fable" only when the task genuinely needs that tier — '
            "passing it explicitly IS the sign-off"
        )
    effective_model = str(DEFAULT_MODEL if model is None else model).strip().lower()

    if model is not None and effective_model in ("opus", "fable"):
        note = (
            f'Explicit model="{effective_model}" — allowed (explicit beats '
            "silent). Reminder: workers run Sonnet here; a whole-fleet launch "
            "at this tier should be a deliberate, stated choice."
        )

    name = (tool_input.get("name") or "").strip()
    suggested_name = ""
    if name:
        mapping = slot_map(env)
        valid_slots = {label for _, label in mapping.values()}
        _, expected_slot = mapping.get(effective_model, ("", DEFAULT_MODEL))
        expected_slot = expected_slot or DEFAULT_MODEL

        fixed = name.lower().replace("_", "-")
        if fixed != name:
            defects.append(f"name '{name}' is not kebab-case lowercase")
        if not re.fullmatch(r"[a-z0-9-]+", fixed):
            defects.append(f"name '{name}' has characters outside [a-z0-9-]")
            fixed = re.sub(r"[^a-z0-9-]", "-", fixed).strip("-")

        slot, _, task_part = fixed.partition("-")
        if slot not in valid_slots:
            task_part = fixed  # no slot prefix at all
            defects.append(
                f"name '{name}' lacks its model-slot prefix (live map: "
                f"{describe_map(env)})"
            )
        elif slot != expected_slot:
            defects.append(
                f"name '{name}' claims slot '{slot}' but "
                f'model="{effective_model}" runs slot \'{expected_slot}\' in '
                f"this session (map: {describe_map(env)})"
            )

        task_segs = [s for s in task_part.split("-") if s]
        alpha_words = [s for s in task_segs if re.search(r"[a-z]{3,}", s)]
        if not task_part or len(alpha_words) < 2 or all(
            OPAQUE_SEG.match(s) for s in task_segs
        ):
            defects.append(
                f"task part '{task_part or '(empty)'}' is not descriptive — "
                "name the WORK in plain words (>=2 words), never bare labels "
                "like T1/D-52 (no-opaque-task-labels rule)"
            )
            task_part = "<two-plain-words>"
        suggested_name = f"{expected_slot}-{task_part}"

    if defects:
        corrected = f'model="{effective_model}"' + (
            f", name=\"{suggested_name}\"" if suggested_name else ""
        )
        head = "Agent launch has 1 defect" if len(defects) == 1 else (
            f"Agent launch has {len(defects)} defects"
        )
        listed = "; ".join(f"({i}) {d}" for i, d in enumerate(defects, 1))
        return (
            f"{head} — fix ALL in ONE re-issued call: {corrected} "
            f"(swap the slot prefix if you choose a different model). "
            f"Defects: {listed}.",
            "",
        )
    return "", note


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        deny, note = decide(payload.get("tool_input") or {})
        if deny:
            out = {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": deny,
                }
            }
        elif note:
            out = {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": note,
                }
            }
        else:
            sys.exit(0)
        print(json.dumps(out))
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard must never block a session


if __name__ == "__main__":
    main()
