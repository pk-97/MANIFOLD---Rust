#!/usr/bin/env python3
"""PreToolUse hook for Agent: one-pass launch guard — model tier + teammate naming.

Mechanical name defects are AUTO-FIXED via updatedInput (PreToolUse decision field,
verified against the 2.1.258 hook docs 2026-09-02): casing, stray characters, a
missing or wrong slot prefix. The correct name is derived from the `model` param —
zero judgment — so the hook rewrites it and allows the launch with a note saying
what it did. Two defect classes still deny, because fixing them is a decision, not
a rewrite:

- `model` absent -> deny. The tier is a per-launch call (CLAUDE.md section How every
  session works): "opus" for judgment, "sonnet" for decided directed tasks, "haiku" for
  retrieval and running a given command. A silent inherit of the orchestrator's tier
  double-bills the worker; passing the tier explicitly IS the sign-off.
- task part not descriptive -> deny (the hook will not invent words for you).

Naming rule: name = "<slot>-<descriptive-task>", kebab-case, task part >= 2 plain words
(opaque labels like T1/D-52 denied: no-opaque-task-labels rule). The slot label is
derived AT SPAWN TIME from the backend the harness will actually use: the `model` param
selects a tier slot; the session env (`ANTHROPIC_DEFAULT_<TIER>_MODEL`) says which
backend that slot resolves to. Unset or claude-* -> Anthropic path, label = tier name;
any other backend is labelled by its last dash segment.

gate_runner.py imports slot_map() from this file — it is the single source of truth for
slot labels.

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

# Opaque label segments: T1, D52, D-52, W3, P2, P2G, R1, S8, BUG-NNN-style
# refs — fine INSIDE a descriptive name, denied as the whole task part.
OPAQUE_SEG = re.compile(r"^[a-z]?-?\d+[a-z]?$", re.IGNORECASE)

# Stands in for the tier in a deny's corrected call when `model` is absent;
# choosing it is the launcher's judgment, never the hook's.
TIER_PLACEHOLDER = "<opus|sonnet|haiku>"


def backend_for_slot(model_param: str, env=os.environ) -> tuple[str, str]:
    """(backend model, slot label) for an Agent-tool `model` param.

    Env var unset or claude-* → Anthropic path: the tier name IS the label.
    """
    backend = (env.get(SLOT_ENV.get(model_param, ""), "") or "").strip()
    if not backend or backend.startswith("claude-"):
        return backend or model_param, model_param
    label = re.sub(r"[^a-z0-9]", "", backend.split("-")[-1].lower()) or backend
    return backend, label


def slot_map(env=os.environ) -> dict:
    """{model_param: (backend, label)} for every tier slot, from live env."""
    return {p: backend_for_slot(p, env) for p in SLOT_ENV}


def describe_map(env=os.environ) -> str:
    return ", ".join(
        f"{p}->{label} ({backend})" for p, (backend, label) in slot_map(env).items()
    )


def decide(tool_input: dict, env=os.environ) -> tuple[str, str, dict | None]:
    """(deny_reason, allow_note, fixed_input).

    fixed_input is the full tool_input with a mechanically corrected `name`
    when that is the ONLY defect — main() emits it as updatedInput. Denies
    (fixed_input None) spell the complete corrected call so one retry lands.
    """
    if (tool_input.get("subagent_type") or "").strip().lower() == "fork":
        return "", "", None

    note = ""
    model = tool_input.get("model")
    effective_model = TIER_PLACEHOLDER if model is None else str(model).strip().lower()

    name = (tool_input.get("name") or "").strip()
    expected_slot = ""
    corrected_name = ""
    name_deny = ""
    if name:
        mapping = slot_map(env)
        valid_slots = {label for _, label in mapping.values()}
        _, expected_slot = mapping.get(effective_model, ("", effective_model))
        expected_slot = expected_slot or effective_model

        fixed = re.sub(r"[^a-z0-9-]", "-", name.lower().replace("_", "-")).strip("-")
        slot, _, task_part = fixed.partition("-")
        if slot not in valid_slots:
            task_part = fixed  # no slot prefix at all — prepend expected below

        task_segs = [s for s in task_part.split("-") if s]
        alpha_words = [s for s in task_segs if re.search(r"[a-z]{3,}", s)]
        if not task_part or len(alpha_words) < 2 or all(
            OPAQUE_SEG.match(s) for s in task_segs
        ):
            name_deny = (
                f"task part '{task_part or '(empty)'}' is not descriptive — "
                "name the WORK in plain words (>=2 words), never bare labels "
                "like T1/D-52 (no-opaque-task-labels rule)"
            )
            task_part = "<two-plain-words>"
        corrected = f"{expected_slot}-{task_part}"
        if corrected != name:
            corrected_name = corrected

    denies: list[str] = []
    if model is None:
        denies.append(
            "no explicit `model` — it would silently inherit the "
            "orchestrator's tier and double-bill the worker. Pick the tier: "
            '"opus" for judgment (diagnosis, GPU, design, reviews, audits), '
            '"sonnet" for decided directed tasks, "haiku" for retrieval and '
            "running a given command — passing it explicitly IS the sign-off"
        )
    if name_deny:
        denies.append(name_deny + f" (live map: {describe_map(env)})")

    if denies:
        corrected_call = f'model="{effective_model}"' + (
            f', name="{corrected_name}"' if corrected_name else ""
        )
        head = "Agent launch has 1 defect" if len(denies) == 1 else (
            f"Agent launch has {len(denies)} defects"
        )
        listed = "; ".join(f"({i}) {d}" for i, d in enumerate(denies, 1))
        return (
            f"{head} — fix ALL in ONE re-issued call: {corrected_call} "
            f"(swap the slot prefix if you choose a different model). "
            f"Defects: {listed}.",
            "",
            None,
        )

    if corrected_name:
        fixed_input = dict(tool_input)
        fixed_input["name"] = corrected_name
        auto_note = (
            f"agent-launch-guard auto-fixed name '{name}' -> '{corrected_name}' "
            f'(model="{effective_model}" runs slot \'{expected_slot}\' here; '
            "the prefix is derived, not chosen)"
        )
        return "", f"{note} {auto_note}".strip(), fixed_input
    return "", note, None


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool_input = payload.get("tool_input") or {}
        deny, note, fixed_input = decide(tool_input)
        if deny:
            out = {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": deny,
                }
            }
        elif fixed_input is not None or note:
            spec = {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": note,
            }
            if fixed_input is not None:
                # Full corrected input, not a patch — safe whether the harness
                # merges updatedInput or replaces tool_input wholesale.
                spec["updatedInput"] = fixed_input
            out = {"hookSpecificOutput": spec}
        else:
            sys.exit(0)
        print(json.dumps(out))
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard must never block a session


if __name__ == "__main__":
    main()
