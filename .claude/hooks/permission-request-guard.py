#!/usr/bin/env python3
"""PermissionRequest + PermissionDenied hook: no worker seat ever prompts Peter, and
every prompt or classifier denial lands in telemetry.

Why a PermissionRequest hook and not the PreToolUse dispatcher: a prompt reaches Peter
through several paths the PreToolUse hooks never see — the classifier pausing auto
mode after repeated blocks (3 in a row / 20 per session, not configurable), the
harness's own redirect / symlink / protected-path checks, `permissions.ask` rules,
critical-path `rm` countdowns. PermissionRequest is the one event the harness fires
for ALL of them, right before the terminal prompt, and it may answer allow/deny.
(docs: hooks#permissionrequest, permission-modes#when-auto-mode-falls-back.)

Policy:
  - Worker seat (payload carries `agent_id` / `agent_type` / `teammate_name`; lead
    payloads carry none — same discriminator as hook_telemetry.worker_no_prompt): DENY with a
    reroute note. A worker that cannot proceed without a human stops and reports up;
    the lead or Peter decides. Nothing here ever ALLOWS — this hook never widens
    what runs, so docs/PERMISSION_BOUNDARY.md section 4 is untouched.
  - Lead seat: no verdict (the prompt proceeds). The lead's prompts are the
    human-decision residue; they are logged so the next inventory starts from data.
  - PermissionDenied (observe-only event): logged only. Never sets `retry`.

Telemetry: one line per event appended to .claude/telemetry/hook-fires.jsonl in the
same shape hook_telemetry.py writes (hook_telemetry's own line carries the verdict;
this hook adds `prompt_source` so prompts are countable without replaying). Logging
never changes a verdict.

Fails OPEN on any error — an exception yields no output, so the harness prompts as
it would have without the hook. Opening a prompt is the safe failure for a guard
whose job is to stop prompts; silently allowing would not be.

Obsolete when: the harness gains a per-agent "never prompt, deny instead" switch
(a `dontAsk`-equivalent that subagents can be spawned with).
"""
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

_LOG = Path(__file__).resolve().parent.parent / "telemetry" / "hook-fires.jsonl"

REROUTE = (
    "WORKER SEAT: this action needs a human permission prompt, and worker seats never "
    "prompt Peter (an unanswered prompt stalls the whole unattended run). Denied by "
    "permission-request-guard.py. Do NOT work around it. Either (a) rewrite the action "
    "into a pre-approved shape — read-only tools, `git -C`/`cargo --manifest-path` "
    "workflow commands, Edit/Write inside your worktree or the session scratchpad, "
    "redirects only to a literal unquoted /tmp/... path, repo scripts run directly "
    "(`scripts/x.py`, never via python3), no `sed w`, no `tee`, no `cd` — or (b) stop "
    "and report the exact command and this denial text up to the lead. "
)


def _ident(payload: dict) -> str:
    for k in ("agent_id", "teammate_name", "agent_type"):
        v = payload.get(k)
        if v:
            return re.sub(r"[^A-Za-z0-9_-]", "_", str(v))[:80]
    return ""


def _summ(tool_input) -> str:
    if not isinstance(tool_input, dict):
        return ""
    for k in ("command", "file_path", "url", "pattern"):
        v = tool_input.get(k)
        if isinstance(v, str) and v:
            return v[:500]
    return json.dumps(tool_input)[:200]


def _log(payload: dict, event: str, verdict: str, who: str) -> None:
    try:
        rec = {
            "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "hook": "permission-request-guard.py",
            "event": event,
            "prompt_source": event,
            "seat": "worker" if who else "lead",
            "verdict": verdict,
            "tool_name": payload.get("tool_name"),
            "permission_mode": payload.get("permission_mode"),
            "cmd": _summ(payload.get("tool_input")),
        }
        if who:
            rec["agent"] = who
        sugg = payload.get("permission_suggestions")
        if isinstance(sugg, list) and sugg:
            rec["suggestions"] = [s.get("rule") for s in sugg if isinstance(s, dict)][:5]
        for k in ("reason", "denial_reason", "message"):
            v = payload.get(k)
            if isinstance(v, str) and v:
                rec["reason"] = v[:300]
                break
        _LOG.parent.mkdir(parents=True, exist_ok=True)
        with open(_LOG, "a") as f:
            f.write(json.dumps(rec, sort_keys=True) + "\n")
    except Exception:
        pass


def decide(payload: dict):
    """Return the JSON to print, or None for no verdict. Pure; used by the tests."""
    event = payload.get("hook_event_name", "")
    who = _ident(payload)
    if event == "PermissionDenied":
        return None
    if event != "PermissionRequest":
        return None
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
    except Exception:
        return
    try:
        out = decide(payload)
        event = payload.get("hook_event_name", "")
        who = _ident(payload)
        verdict = "deny" if out else ("logged" if event == "PermissionDenied" else "prompt")
        _log(payload, event, verdict, who)
        if out:
            print(json.dumps(out))
    except Exception:
        return  # fail open: the prompt proceeds


if __name__ == "__main__":
    main()
