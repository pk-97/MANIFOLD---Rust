#!/usr/bin/env python3
"""PreToolUse hook for Agent: require an explicit worker model.

Why this exists (2026-07-06, Peter): an orchestrator launched two worker
agents with no `model` param, silently inheriting the orchestrator's own
tier (Fable) — double-billing every worker token. The daemon's
anchor/agent-model-discipline note fires only AFTER launch; nothing
prevented it. House rule: workers run Sonnet; anything above is an explicit,
per-launch decision.

Behavior (deterministic, no model calls):
- `model` present → allow. "sonnet"/"haiku" allow silently; "opus"/"fable"
  allow WITH a reminder attached (an explicit param is a visible decision —
  the hook's job is killing the silent default, not vetoing judgment).
- `model` absent → deny with the reason below, EXCEPT subagent_type "fork"
  (forks always inherit; the param is documented as ignored there).

Fails open on any error: this hook must never be able to block a session.
"""
import json
import sys


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool_input = payload.get("tool_input") or {}
        model = tool_input.get("model")
        subagent = (tool_input.get("subagent_type") or "").strip().lower()

        if subagent == "fork":
            sys.exit(0)

        if model is None:
            print(
                json.dumps(
                    {
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": (
                                "Agent launch without an explicit `model` param — it would "
                                "silently inherit the orchestrator's tier and double-bill the "
                                "worker (2026-07-06 incident). Re-issue the SAME call with "
                                "`model` set: \"sonnet\" for workers (house default), or "
                                "\"opus\"/\"fable\" only when the task genuinely needs that "
                                "tier — passing it explicitly IS the sign-off."
                            ),
                        }
                    }
                )
            )
            sys.exit(0)

        if str(model).strip().lower() in ("opus", "fable"):
            print(
                json.dumps(
                    {
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "allow",
                            "permissionDecisionReason": (
                                f"Explicit model=\"{model}\" — allowed (explicit beats silent). "
                                "Reminder: workers run Sonnet here; a whole-fleet launch at "
                                "this tier should be a deliberate, stated choice."
                            ),
                        }
                    }
                )
            )
            sys.exit(0)

        sys.exit(0)
    except Exception:
        # Fail open — a guard hook must never be able to block the session.
        sys.exit(0)


if __name__ == "__main__":
    main()
