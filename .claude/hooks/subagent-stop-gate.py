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
subagent. MAX_BLOCKS (3) consecutive blocks per agent_id then allows
through with a loud systemMessage.

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
"""

import json
import os
import re
import subprocess
import sys

MAX_BLOCKS = 3
PAYLOAD_LOG = "/tmp/manifold_subagent_stop_payloads.jsonl"
STATE_FILE = "/tmp/subagent_stop_gate_state.json"

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
GATE_RUNNER = os.path.join(REPO, "scripts", "gate_runner.py")

# Executor tier regex — mirrors agent-tier-spawn-guard.py exactly
EXECUTOR_TIERS = re.compile(
    r"claude-(sonnet|haiku)|deepseek|kimi-k2|kimi-for-coding", re.IGNORECASE
)

BUG_RE = re.compile(r"BUG-\w+")


def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return default


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

    # ---- Gate failed — check block limit ----
    state = load_json(STATE_FILE, {})
    blocks = int(state.get(agent_id, 0))

    if blocks >= MAX_BLOCKS:
        # Allow through with a loud systemMessage (precedent: lane-report-enforcer)
        print(json.dumps({
            "systemMessage": (
                f"subagent-stop-gate: agent '{agent_id}' ({agent_type}) "
                f"blocked {MAX_BLOCKS}x for red gates on {task_id} — "
                "allowed through; check the gate output and lane discipline."
            )
        }))
        state[agent_id] = 0
        try:
            with open(STATE_FILE, "w") as f:
                json.dump(state, f)
        except Exception:
            pass
        return 0

    # ---- Block the stop: exit 2 with gate failure output as feedback ----
    state[agent_id] = blocks + 1
    try:
        with open(STATE_FILE, "w") as f:
            json.dump(state, f)
    except Exception:
        pass

    # Build feedback: brief summary of what failed
    summary_lines = []
    for line in r.stdout.split("\n"):
        if "FAIL" in line or "failed" in line.lower():
            summary_lines.append(line.strip())
    feedback = (
        f"subagent-stop-gate: gate FAILED for {task_id} (block {blocks + 1}/{MAX_BLOCKS}). "
        f"Gate runner exit {r.returncode}. "
        + ("; ".join(summary_lines[:5]) if summary_lines else r.stdout.strip()[-500:])
    )
    print(feedback, file=sys.stderr)
    sys.exit(2)


if __name__ == "__main__":
    sys.exit(main())
