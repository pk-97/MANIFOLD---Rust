#!/usr/bin/env python3
"""PermissionRequest hook: a worker seat never gets a human permission prompt.

A prompt on an unattended worker stalls the whole run, so a worker prompt is
denied with a reroute note. The worker then either rewrites the action into a
pre-approved shape or stops and reports up to the lead.

Discriminator: the payload carries `agent_id`, `agent_type` or `teammate_name`.
Lead payloads carry none of them, so the lead gets no verdict and the prompt
proceeds as normal.

This hook never allows anything. It only denies, so it cannot widen what runs.

Fails open: bad or empty stdin, or any error, prints nothing and exits 0. The
harness then prompts as it would without the hook.
"""
import json
import re
import sys

REROUTE = (
    "WORKER SEAT: this action needs a human permission prompt, and worker seats never "
    "prompt Peter. Denied by permission-request-guard.py. Do not work around it. "
    "Stop and report the exact action and this denial to the lead. "
)


def _ident(payload: dict) -> str:
    for k in ("agent_id", "teammate_name", "agent_type"):
        v = payload.get(k)
        if v:
            return re.sub(r"[^A-Za-z0-9_-]", "_", str(v))[:80]
    return ""


def decide(payload: dict):
    """Return the JSON to print, or None for no verdict. Pure; used by the tests."""
    if payload.get("hook_event_name") != "PermissionRequest":
        return None
    who = _ident(payload)
    if not who:
        return None
    tool = payload.get("tool_name", "tool")
    return {
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "deny",
                "message": REROUTE + f"(seat {who}, tool {tool}).",
            },
        }
    }


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        if not isinstance(payload, dict):
            return
        out = decide(payload)
    except Exception:
        return  # fail open: the prompt proceeds
    if out:
        print(json.dumps(out))


if __name__ == "__main__":
    main()
