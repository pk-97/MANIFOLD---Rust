#!/usr/bin/env python3
"""SubagentStop hook: per-lane gate firing for executor-tier stops (P5).

Per GATE_RUNTIME_DESIGN.md P5: when an executor-tier subagent stops,
run gate_runner per-lane against the lane's brief and block the stop
if any gate fails (red gates). Non-executor tiers pass through.

Confidence-gated (BUG-og15 binding contract): fires only when the payload
fields it needs (agent_id, agent_type) are present. On unknown shapes or
missing task/brief identification, allows the stop and logs the payload
to /tmp/manifold_subagent_stop_payloads.jsonl for empirical documentation.

Blocking mechanism (precedent: lane-report-enforcer.py): exit 2 with
stderr message blocks the stop and sends the message as feedback to the
subagent. The block counter IS the verdict trail (2026-07-27, replacing a
private /tmp state file): the task's trailing streak of red per-lane
verdicts — which gate_runner just appended to — decides. Streak past
FAIL_STREAK_LIMIT allows through with a loud systemMessage, so the trail
is the single fact both this hook and gate_runner's stop-retrying
directive read; two counters can no longer drift.

Payload schema (empirically verified 2026-07-25, claude CLI 2.1.219):
  hook_event_name: "SubagentStop"
  stop_hook_active: bool — when true, exit 0 immediately (system has decided)
  agent_id: str
  agent_transcript_path: str
  agent_type: str
  last_assistant_message: str (optional)
  ...plus standard session fields from Kf

Executor tiers (from agent-tier-spawn-guard.py):
  claude-sonnet, claude-haiku, deepseek*, kimi-k2*, kimi-for-coding

Obsolete when: the routing policy in docs/AGENT_ROUTING.md retires the two-tier lead/lane model this guard polices; recheck at each routing-policy revision.
"""

import json
import os
import re
import subprocess
import sys

PAYLOAD_LOG = "/tmp/manifold_subagent_stop_payloads.jsonl"

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
GATE_RUNNER = os.path.join(REPO, "scripts", "gate_runner.py")


def fail_streak(task_id):
    """Trailing consecutive red per-lane verdicts for a task, read from the
    trail gate_runner just appended to. Mirrors gate_runner._fail_streak but
    stays subprocess-clean (importing gate_runner executes its module-level
    guard loading). Unreadable trail → 0 (fail open)."""
    verdicts_dir = os.environ.get("GATE_RUNNER_VERDICTS_DIR") or os.path.join(
        REPO, ".claude", "orchestration", "verdicts")
    path = os.path.join(verdicts_dir, f"{task_id}.jsonl")
    streak = 0
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                v = json.loads(line)
                if v.get("phase") != "per-lane" or v.get("kind") != "gate":
                    continue
                streak = 0 if v.get("pass") else streak + 1
    except Exception:
        return 0
    return streak


def fail_streak_limit():
    """Read FAIL_STREAK_LIMIT from gate_runner's source so the two callers
    of the trail share one constant. Falls back to 3."""
    try:
        with open(GATE_RUNNER) as f:
            m = re.search(r"^FAIL_STREAK_LIMIT\s*=\s*(\d+)", f.read(), re.MULTILINE)
        if m:
            return int(m.group(1))
    except Exception:
        pass
    return 3

# Executor tier regex — mirrors agent-tier-spawn-guard.py exactly
EXECUTOR_TIERS = re.compile(
    r"claude-(sonnet|haiku)|deepseek|kimi-k2|kimi-for-coding", re.IGNORECASE
)

BUG_RE = re.compile(r"BUG-\w+")


def find_task_and_brief(payload, log_fp):
    """Try multiple strategies to find task ID and brief path from payload.

    Strategy 1: Search entire payload JSON string for BUG-xxx.
    Strategy 2: Search last_assistant_message for BUG-xxx and .md paths.
    Strategy 3: Read agent_transcript_path tail (last 100KB) for BUG-xxx
                and .md brief paths.
    Strategy 4: Search all payload values for .md paths.

    Returns (task_id, brief_path). Either may be None.
    """
    task_id = None
    brief_path = None

    payload_str = json.dumps(payload)

    # Strategy 1: payload-wide BUG-xxx search
    m = BUG_RE.search(payload_str)
    if m:
        task_id = m.group(0)

    # Strategy 2: last_assistant_message
    lam = payload.get("last_assistant_message") or ""
    if not task_id and lam:
        m = BUG_RE.search(lam)
        if m:
            task_id = m.group(0)

    # Also search lam for brief paths
    if lam:
        for part in re.findall(r'(\S+\.md)', lam):
            candidate = part if part.startswith("/") else os.path.join(REPO, part)
            if os.path.exists(candidate):
                brief_path = candidate
                break

    # Strategy 3: transcript tail
    tp = payload.get("agent_transcript_path") or ""
    if tp and os.path.exists(tp):
        try:
            with open(tp, "rb") as f:
                f.seek(0, 2)
                size = f.tell()
                read_size = min(size, 100 * 1024)
                f.seek(max(0, size - read_size))
                tail = f.read().decode("utf-8", errors="replace")

            if not task_id:
                m = BUG_RE.search(tail)
                if m:
                    task_id = m.group(0)

            if not brief_path:
                for part in re.findall(r'(\S+\.md)', tail):
                    candidate = part if part.startswith("/") else os.path.join(REPO, part)
                    if os.path.exists(candidate):
                        brief_path = candidate
                        break
        except Exception as e:
            log_fp.write(f"transcript read error: {e}\n")

    # Strategy 4: payload values
    if not brief_path:
        for v in payload.values():
            if isinstance(v, str) and v.endswith(".md"):
                candidate = v if v.startswith("/") else os.path.join(REPO, v)
                if os.path.exists(candidate):
                    brief_path = candidate
                    break

    return task_id, brief_path


def append_payload_log(payload):
    """Append full payload to the JSONL log for empirical documentation."""
    try:
        with open(PAYLOAD_LOG, "a") as f:
            record = {
                "ts": __import__("datetime").datetime.now(
                    __import__("datetime").timezone.utc
                ).isoformat(),
                "payload": payload,
            }
            f.write(json.dumps(record, sort_keys=True) + "\n")
    except Exception:
        pass


def main():
    # ---- Parse payload (fail OPEN) ----
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return 0

    # ---- stop_hook_active: system has decided, exit 0 ----
    if payload.get("stop_hook_active"):
        return 0

    # ---- Extract agent info ----
    agent_id = payload.get("agent_id") or ""
    agent_type = payload.get("agent_type") or ""
    if not agent_id:
        append_payload_log(payload)
        return 0

    # ---- Tier check: only executor tiers are gated ----
    if not EXECUTOR_TIERS.search(agent_type):
        return 0

    # ---- Open the payload log for this attempt ----
    log_fp = None
    try:
        log_fp = open(PAYLOAD_LOG, "a")
    except Exception:
        pass

    # ---- Find task and brief (confidence-gated) ----
    task_id, brief_path = find_task_and_brief(payload, log_fp or sys.__stdout__)

    if not task_id or not brief_path:
        # Cannot identify — allow + log
        if log_fp:
            log_fp.write(
                json.dumps({
                    "ts": __import__("datetime").datetime.now(
                        __import__("datetime").timezone.utc
                    ).isoformat(),
                    "event": "subagent-stop: unknown payload shape",
                    "agent_id": agent_id,
                    "agent_type": agent_type,
                    "found_task": bool(task_id),
                    "found_brief": bool(brief_path),
                    "payload": payload,
                }, sort_keys=True) + "\n"
            )
            log_fp.close()
        else:
            append_payload_log(payload)
        return 0

    # ---- Run gate_runner per-lane ----
    try:
        r = subprocess.run(
            [sys.executable, GATE_RUNNER, "per-lane",
             "--task", task_id,
             "--brief", brief_path],
            capture_output=True, text=True, timeout=300,
        )
    except subprocess.TimeoutExpired:
        # Fail OPEN on timeout
        if log_fp:
            log_fp.close()
        return 0
    except Exception:
        # Fail OPEN on any error
        if log_fp:
            log_fp.close()
        return 0

    if log_fp:
        log_fp.close()

    # ---- Gate passed — allow ----
    if r.returncode == 0:
        return 0

    # ---- Gate failed — the verdict trail is the block counter ----
    streak = fail_streak(task_id)
    limit = fail_streak_limit()

    if streak > limit:
        # Allow through with a loud systemMessage (precedent: lane-report-enforcer)
        print(json.dumps({
            "systemMessage": (
                f"subagent-stop-gate: task {task_id} has {streak} consecutive "
                f"red per-lane verdicts (limit {limit}) — agent '{agent_id}' "
                f"({agent_type}) allowed through; the lane owes a blocked "
                "report, and the trail has the gate output."
            )
        }))
        return 0

    # ---- Block the stop: exit 2 with gate failure output as feedback ----
    summary_lines = []
    for line in r.stdout.split("\n"):
        if "FAIL" in line or "failed" in line.lower():
            summary_lines.append(line.strip())
    feedback = (
        f"subagent-stop-gate: gate FAILED for {task_id} (red run {streak}/{limit}). "
        f"Gate runner exit {r.returncode}. "
        + ("; ".join(summary_lines[:5]) if summary_lines else r.stdout.strip()[-500:])
    )
    print(feedback, file=sys.stderr)
    sys.exit(2)


if __name__ == "__main__":
    sys.exit(main())
