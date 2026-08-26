#!/usr/bin/env python3
"""PreToolUse hook for Agent: background spawns require an armed lane health check.

Rule (docs/AGENT_ROUTING.md — "Lane health is scheduled, never Peter's job"): whenever
the lead runs Agent-tool lanes detached, a lane health check must be armed first.

Mechanic (2026-08-26, k3 lead, Peter-approved): this CC build removed durable cron —
CronCreate's durable flag is gone and ALL jobs are session-only in the harness's
in-memory store, which a hook process cannot read. Verification of arming is therefore
impossible from here, so the hook WARNS instead of denying: every background spawn
without a parseable armed marker in .claude/scheduled_tasks.json passes with an
additionalContext reminder carrying the exact session CronCreate to arm. The actual
protection lives in the lead honoring that reminder (the session job fires while the
session is idle) plus completion notifications. A legacy durable marker in the store
still counts as armed and silences the warning (forward-compat if durability returns).

Scope: fires only on BACKGROUND spawns — tool_input.run_in_background absent (the
harness default) or true. Explicit run_in_background=false passes silently: a
synchronous agent cannot stall silently. cc-fleet subagents/teammates are headless CLI
runs, not Agent-tool calls — unaffected.

Fail modes: hook-internal errors fail open — a guard must never block a session on its
own bug.

Obsolete when: the harness exposes session cron state to hooks (then arming can be
verified and the deny restored), or the lane-health rule in AGENT_ROUTING.md is
retired.
"""
import json
import os
import sys

MARKER = "lane-health-check"

CORRECTED_FORM = (
    "arm a SESSION recurring CronCreate whose prompt contains the literal string "
    f"'{MARKER}', e.g. CronCreate(cron='7-59/10 * * * *', "
    f"prompt='{MARKER}: per lane — is a build/test process running; has the worktree "
    "moved (files, commits)? two consecutive idle checks with no report = stalled: "
    "message once, then stop the lane and escalate per the seat ladder. No lane "
    "processes AND no live lane to escalate = the fleet is down: CronDelete this job "
    "(re-arm on the next spawn)'). Durable cron no longer exists in this harness "
    "build — the session job is the enforcement; it dies with the session, so re-arm "
    "whenever this reminder appears. Delete it only when the fleet is idle for good."
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
    """Warning reason, or '' when armed/sync. Only background spawns are checked.

    Never denies: durable cron is gone from the harness, so an empty or missing
    store is indistinguishable from a correctly armed session job. The warning
    carries the exact session CronCreate so the lead can self-correct.
    """
    if tool_input.get("run_in_background") is False:
        return ""
    store = find_store(candidate_dirs)
    if store is not None:
        armed, problem = armed_job_present(store)
        if armed:
            return ""
        if problem:
            return (
                f"lane-health-guard: {store} {problem} — no verifiable health check. "
                f"If you have not armed one this session, {CORRECTED_FORM}"
            )
    return (
        "lane-health-guard: no verifiable lane health check (session cron is "
        "invisible to hooks in this build). If you have not armed one this session, "
        f"{CORRECTED_FORM}"
    )


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        candidates = [
            str(payload.get("cwd") or ""),
            os.environ.get("CLAUDE_PROJECT_DIR", ""),
            os.getcwd(),
        ]
        warning = decide(payload.get("tool_input") or {}, candidates)
        if not warning:
            sys.exit(0)
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "additionalContext": warning,
                    }
                }
            )
        )
        sys.exit(0)
    except Exception:
        sys.exit(0)  # fail open — a guard must never block a session


if __name__ == "__main__":
    main()
