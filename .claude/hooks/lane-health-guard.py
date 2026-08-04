#!/usr/bin/env python3
"""PreToolUse hook for Agent: background spawns require an armed lane health check.

Rule (docs/AGENT_ROUTING.md — "Lane health is scheduled, never Peter's job"): whenever
the lead runs Agent-tool lanes detached, a lane health check must be armed first. Today
that relies on the lead remembering; this hook makes forgetting a deterministic deny.

Mechanic: session-only CronCreate jobs live in the harness's in-memory store and are
INVISIBLE outside the session, so the enforced form is a DURABLE job — durable jobs
persist to .claude/scheduled_tasks.json (schema: {"tasks": [{id, cron, prompt,
createdAt, recurring?, ...}]}; the harness reads a missing or unparseable file as zero
tasks). A job counts as armed when some task has `recurring: true` and its prompt
contains the literal marker string MARKER.

Scope: fires only on BACKGROUND spawns — tool_input.run_in_background absent (the
harness default) or true. Explicit run_in_background=false passes: a synchronous agent
cannot stall silently. No subagent_type exemptions: a background fork or consult seat
stalls the same as any lane. cc-fleet subagents/teammates are headless CLI runs, not
Agent-tool calls — unaffected (their --timeout/--max-budget-usd bound silent failure).

Fail modes: missing store, zero matching tasks, or an unparseable store all DENY —
an unparseable store is indistinguishable from an unarmed one (the harness itself reads
it as zero jobs), so the message says so and names the fix. Unexpected hook-internal
errors fail open: a guard must never be able to block a session on its own bug.

Store location: the payload's cwd is tried first, then $CLAUDE_PROJECT_DIR, then the
process cwd — first dir whose .claude/scheduled_tasks.json exists wins. That mirrors
the harness, which reads exactly one store relative to the project dir.

Obsolete when: the harness exposes session cron state to hooks (then session-only
arming can count too), or the lane-health rule in AGENT_ROUTING.md is retired.
"""
import json
import os
import sys

MARKER = "lane-health-check"

CORRECTED_FORM = (
    "arm a durable recurring CronCreate whose prompt contains the literal string "
    f"'{MARKER}', e.g. CronCreate(cron='7-59/10 * * * *', durable=true, "
    f"prompt='{MARKER}: per lane — is a build/test process running; has the worktree "
    "moved (files, commits)? two consecutive idle checks with no report = stalled: "
    "message once, then stop the lane and escalate per the seat ladder. No lane "
    "processes AND no live lane to escalate = the fleet is down: CronDelete this job "
    "(re-arming is hook-enforced on the next spawn)'), then respawn "
    "the Agent call. Session-only (durable=false) jobs are invisible to this hook. "
    "The self-cancel clause is mandatory: a durable job outlives the session that "
    "armed it, and an orphaned check burns a lead wakeup every 10 minutes "
    "(2026-08-04, fired all morning against an empty fleet)."
)


def find_store(candidate_dirs: list[str]) -> str | None:
    """First candidate's .claude/scheduled_tasks.json that exists, else None."""
    for d in candidate_dirs:
        if not d:
            continue
        path = os.path.join(d, ".claude", "scheduled_tasks.json")
        if os.path.isfile(path):
            return path
    return None


def armed_job_present(store_path: str) -> tuple[bool, str]:
    """(armed, problem). problem is '' when the store parsed cleanly."""
    try:
        with open(store_path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        return False, f"store is unparseable ({e})"
    tasks = data.get("tasks") if isinstance(data, dict) else None
    if not isinstance(tasks, list):
        return False, "store has no 'tasks' list"
    for task in tasks:
        if (
            isinstance(task, dict)
            and task.get("recurring") is True
            and MARKER in str(task.get("prompt") or "")
        ):
            return True, ""
    return False, ""


def decide(tool_input: dict, candidate_dirs: list[str]) -> str:
    """Deny reason, or '' to allow. Only background spawns are checked."""
    if tool_input.get("run_in_background") is False:
        return ""
    store = find_store(candidate_dirs)
    if store is None:
        checked = ", ".join(
            os.path.join(d or ".", ".claude", "scheduled_tasks.json")
            for d in candidate_dirs
            if d
        ) or ".claude/scheduled_tasks.json"
        return (
            "Background Agent spawn denied: no lane health check is armed — no durable "
            f"cron store exists (checked: {checked}). The lane-health rule "
            "(docs/AGENT_ROUTING.md — lane health is scheduled, never Peter's job) is "
            f"hook-enforced: {CORRECTED_FORM}"
        )
    armed, problem = armed_job_present(store)
    if armed:
        return ""
    if problem:
        return (
            f"Background Agent spawn denied: {store} {problem} — the harness reads a "
            "corrupt store as zero jobs, so any health check you armed is already lost. "
            f"Fix or delete the file, then {CORRECTED_FORM}"
        )
    return (
        "Background Agent spawn denied: no lane health check is armed — "
        f"{store} has no recurring task whose prompt contains '{MARKER}'. The "
        "lane-health rule (docs/AGENT_ROUTING.md — lane health is scheduled, never "
        f"Peter's job) is hook-enforced: {CORRECTED_FORM}"
    )


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        candidates = [
            str(payload.get("cwd") or ""),
            os.environ.get("CLAUDE_PROJECT_DIR", ""),
            os.getcwd(),
        ]
        deny = decide(payload.get("tool_input") or {}, candidates)
        if not deny:
            sys.exit(0)
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": deny,
                    }
                }
            )
        )
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard must never block a session


if __name__ == "__main__":
    main()
