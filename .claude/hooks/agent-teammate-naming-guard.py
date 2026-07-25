#!/usr/bin/env python3
"""PreToolUse hook for Agent: enforce teammate naming conventions.

Why (2026-07-25, Peter): "the hook needs to enforce all of the correct
naming and conventions for teammates such as their model name and role."
Soft rules lost: the lead spawned an invisible, unnamed-by-convention
background agent within a minute of its first lane task this session, and
opaque task labels (T1, D-52) were already a standing Peter rule
(memory: no-opaque-task-labels) with no machinery behind it.

The convention (mirrors the model slot map, docs/AGENT_ROUTING.md):
  name = "<slot>-<descriptive-task>"
  slot <- model arg:  haiku->flash  sonnet->glm47  opus->glm52  fable->k3
  kebab-case; the task part must be descriptive — at least two alphabetic
  words; opaque label segments (T1, D52, W3, P2-G style) are denied.

A name that carries the slot makes every panel entry, inbox message, and
litellm ledger line self-describing: model AND role at a glance.

Also enforced: explicit `subagent_type` (Peter found the split case
2026-07-25): spawns that omit it run fine but write NO agentType into the
team file, and the teammate panel row keys off that field — the lane is
invisible. Every spawn today did this; the 07-21/07-22 spawns that rendered
all had agentType set.

Behavior (deterministic, no model calls):
- subagent_type "fork" or a missing name -> allow (nameless spawns are the
  model guard's / harness's concern; this hook only judges names it can see).
- Missing subagent_type, wrong/missing slot prefix, bad casing, opaque task
  part -> deny with the fix spelled out.

Fails open on any error: a guard hook must never be able to block a session.
"""
import json
import re
import sys

SLOT_FOR_MODEL = {
    "haiku": "flash",
    "sonnet": "glm47",
    "opus": "glm52",
    "fable": "k3",
}
VALID_SLOTS = sorted(set(SLOT_FOR_MODEL.values()))

# Opaque label segments: T1, D52, D-52, W3, P2, P2G, R1, S8, BUG-NNN-style
# refs — fine INSIDE a descriptive name, denied as the whole task part.
OPAQUE_SEG = re.compile(r"^[a-z]?-?\d+[a-z]?$", re.IGNORECASE)


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
    sys.exit(0)


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool_input = payload.get("tool_input") or {}

        if (tool_input.get("subagent_type") or "").strip().lower() == "fork":
            sys.exit(0)

        name = (tool_input.get("name") or "").strip()
        if not name:
            sys.exit(0)  # nothing to judge; harness/hooks cover nameless spawns

        subagent_type = (tool_input.get("subagent_type") or "").strip()
        if not subagent_type:
            deny(
                f"Teammate spawn '{name}' has no subagent_type — the harness runs "
                "general-purpose but writes NO agentType into the team file, and the "
                "teammate panel row keys off that field: the lane is invisible "
                "(split case found by Peter 2026-07-25). Re-issue with "
                'subagent_type: "general-purpose".'
            )

        model = str(tool_input.get("model") or "").strip().lower()
        expected_slot = SLOT_FOR_MODEL.get(model)

        if name != name.lower() or not re.fullmatch(r"[a-z0-9-]+", name):
            deny(
                f"Teammate name '{name}' violates the naming convention: "
                "kebab-case lowercase only. Format: <slot>-<descriptive-task>, "
                "e.g. flash-beads-migration."
            )

        slot, _, task_part = name.partition("-")
        if slot not in VALID_SLOTS:
            deny(
                f"Teammate name '{name}' must start with its model slot: "
                f"one of {', '.join(VALID_SLOTS)}- (haiku->flash, sonnet->glm47, "
                f"opus->glm52, fable->k3). Rename, e.g. "
                f"'{expected_slot or 'flash'}-{name}'."
            )

        if expected_slot and slot != expected_slot:
            deny(
                f"Teammate name '{name}' claims slot '{slot}' but model=\"{model}\" "
                f"maps to slot '{expected_slot}'. Name and model must agree: "
                f"'{expected_slot}-{task_part}'."
            )

        task_segs = [s for s in task_part.split("-") if s]
        alpha_words = [s for s in task_segs if re.search(r"[a-z]{3,}", s)]
        if not task_part or len(alpha_words) < 2 or all(
            OPAQUE_SEG.match(s) for s in task_segs
        ):
            deny(
                f"Teammate name '{name}': task part '{task_part or '(empty)'}' is not "
                "descriptive. Name the WORK in plain words (>=2 words), never bare "
                "labels like T1/D-52 (Peter's no-opaque-task-labels rule). "
                f"e.g. '{slot}-migrate-bug-backlog'."
            )

        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open


if __name__ == "__main__":
    main()
