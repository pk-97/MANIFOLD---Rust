#!/usr/bin/env python3
"""Synthetic-payload tests for subagent-stop-gate.py (P5 gate).

Each test feeds a synthetic SubagentStop payload via stdin to the hook
and checks the exit code, stderr, and verdict trail. Uses a sandboxed
GATE_RUNNER_VERDICTS_DIR and temp state/payload files.

Test matrix:
  T1: Executor, gates pass  → exit 0, verdict appended
  T2: Executor, gates fail  → exit 2, stderr feedback
  T3: Unknown payload shape → exit 0, log line written
  T4: Non-executor model   → exit 0
  T5: trail streak > limit → exit 0, systemMessage
  T6: stop_hook_active     → exit 0 immediately

Usage: python3 .claude/hooks/test_subagent_stop_gate.py
"""

import json
import os
import subprocess
import sys
import tempfile

HOOK = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "subagent-stop-gate.py",
)
REPO = os.path.normpath(os.path.join(os.path.dirname(HOOK), "..", ".."))
GATE_RUNNER = os.path.join(REPO, "scripts", "gate_runner.py")

PASSED = 0
FAILED = 0
PAYLOAD_LOG = "/tmp/manifold_subagent_stop_payloads.jsonl"


def clean_state():
    """Remove the payload log between tests (the block counter lives in the
    verdict trail, sandboxed per-test via GATE_RUNNER_VERDICTS_DIR)."""
    try:
        os.remove(PAYLOAD_LOG)
    except FileNotFoundError:
        pass


def ok(label):
    global PASSED
    PASSED += 1
    print(f"  PASS: {label}")


def fail(label, detail=""):
    global FAILED
    FAILED += 1
    msg = f"  FAIL: {label}"
    if detail:
        msg += f" — {detail}"
    print(msg)


def run_hook(payload, env_override=None):
    """Pipe a JSON payload to subagent-stop-gate.py, return (rc, stdout, stderr)."""
    env = {**os.environ, **(env_override or {})}
    r = subprocess.run(
        [sys.executable, HOOK],
        input=json.dumps(payload),
        capture_output=True, text=True, timeout=30,
        env=env,
    )
    return r.returncode, r.stdout, r.stderr


def make_payload(overrides=None):
    """Build a synthetic SubagentStop payload. Overrides merge in."""
    base = {
        "hook_event_name": "SubagentStop",
        "stop_hook_active": False,
        "agent_id": "test-agent-deepseek-abc123",
        "agent_transcript_path": "/tmp/fake_transcript.jsonl",
        "agent_type": "deepseek-v4-flash",
        "last_assistant_message": "Gate test BUG-og15 pass",
    }
    if overrides:
        base.update(overrides)
    return base


def write_brief(path, gates):
    """Write a brief markdown file with the given gate commands."""
    with open(path, "w") as f:
        f.write(f"# Test brief for BUG-og15\n\n**Gate:**\n")
        for g in gates:
            f.write(f"`{g}`\n")


def count_verdicts(task_id, verdicts_dir):
    """Count verdict lines for a task."""
    p = os.path.join(verdicts_dir, f"{task_id}.jsonl")
    if not os.path.exists(p):
        return 0
    with open(p) as f:
        return sum(1 for line in f if line.strip())


def test_1_executor_pass():
    """Executor tier, gates pass → exit 0, verdict appended."""
    clean_state()
    with tempfile.TemporaryDirectory() as tmpdir:
        brief = os.path.join(tmpdir, "brief_pass.md")
        write_brief(brief, ["true"])
        verdicts_dir = os.path.join(tmpdir, "verdicts")
        os.makedirs(verdicts_dir)

        payload = make_payload({
            "last_assistant_message": f"Read {brief}. Task: BUG-og15",
            "agent_type": "deepseek-v4-flash",
        })
        rc, stdout, stderr = run_hook(payload, {
            "GATE_RUNNER_VERDICTS_DIR": verdicts_dir,
        })

        if rc == 0:
            ok("T1: exit 0")
        else:
            fail("T1: exit 0", f"got {rc}: {stderr}")

        if count_verdicts("BUG-og15", verdicts_dir) >= 1:
            ok("T1: verdict appended")
        else:
            fail("T1: verdict appended", "no verdict file found")

        if "PASS" in stdout or rc == 0:
            pass  # Good enough with pass check above


def test_2_executor_fail():
    """Executor tier, gates fail → exit 2 with stderr feedback."""
    clean_state()
    with tempfile.TemporaryDirectory() as tmpdir:
        brief = os.path.join(tmpdir, "brief_fail.md")
        write_brief(brief, ["false"])
        verdicts_dir = os.path.join(tmpdir, "verdicts")
        os.makedirs(verdicts_dir)

        payload = make_payload({
            "last_assistant_message": f"Read {brief}. Task: BUG-ogfl",
            "agent_type": "deepseek-v4-flash",
        })
        rc, stdout, stderr = run_hook(payload, {
            "GATE_RUNNER_VERDICTS_DIR": verdicts_dir,
        })

        if rc == 2:
            ok("T2: exit 2 (blocked)")
        else:
            fail("T2: exit 2", f"got {rc}: {stderr}")

        if stderr:
            ok("T2: stderr feedback present")
        else:
            fail("T2: stderr feedback present", "stderr was empty")

        if "FAIL" in stderr or "block" in stderr.lower():
            ok("T2: feedback names gate failure")
        else:
            fail("T2: feedback names gate failure", f"stderr: {stderr[:200]}")


def test_3_unknown_shape():
    """Unknown payload shape (missing agent_id) → exit 0, log line written."""
    clean_state()
    payload = {
        "hook_event_name": "SubagentStop",
        "unknown_field": "value",
        # no agent_id, no agent_type
    }
    rc, stdout, stderr = run_hook(payload)

    if rc == 0:
        ok("T3: exit 0 (fail open)")
    else:
        fail("T3: exit 0", f"got {rc}")

    # Check that payload was logged
    if os.path.exists(PAYLOAD_LOG):
        with open(PAYLOAD_LOG) as f:
            content = f.read()
        if "unknown_field" in content:
            ok("T3: payload logged to JSONL")
        else:
            fail("T3: payload logged", "unknown_field not in log")
    else:
        fail("T3: payload log file", "does not exist")


def test_4_non_executor():
    """Non-executor tier (k3) → exit 0, no gate run."""
    clean_state()
    with tempfile.TemporaryDirectory() as tmpdir:
        verdicts_dir = os.path.join(tmpdir, "verdicts")
        os.makedirs(verdicts_dir)

        payload = make_payload({
            "agent_type": "k3",
            "last_assistant_message": "Task BUG-oglead",
        })
        rc, stdout, stderr = run_hook(payload, {
            "GATE_RUNNER_VERDICTS_DIR": verdicts_dir,
        })

        if rc == 0:
            ok("T4: exit 0 (allowed)")
        else:
            fail("T4: exit 0", f"got {rc}")

        # No verdict should have been written (non-executor bypasses gate)
        if count_verdicts("BUG-oglead", verdicts_dir) == 0:
            ok("T4: no verdict for non-executor")
        else:
            fail("T4: no verdict for non-executor", "verdict was written")


def test_5_max_blocks():
    """Trail streak past the limit → exit 0 with systemMessage on 4th red.

    The counter is the verdict trail (no private state file): prime it with
    3 red per-lane verdicts; this run's red makes 4 > FAIL_STREAK_LIMIT."""
    clean_state()
    with tempfile.TemporaryDirectory() as tmpdir:
        brief = os.path.join(tmpdir, "brief_fail.md")
        write_brief(brief, ["false"])
        verdicts_dir = os.path.join(tmpdir, "verdicts")
        os.makedirs(verdicts_dir)

        agent_id = "test-agent-deepseek-blocked-999"
        red = {
            "schema": 1, "task": "BUG-ogblk", "phase": "per-lane",
            "brief": brief, "branch": "lane/x", "commit": None, "gates": [],
            "scope": {"files_changed": [], "in_scope": True}, "pass": False,
            "kind": "gate", "reason": None, "runner": "gate_runner.py@lead",
            "ts": "2026-07-27T00:00:00+00:00",
        }
        with open(os.path.join(verdicts_dir, "BUG-ogblk.jsonl"), "w") as f:
            for _ in range(3):
                f.write(json.dumps(red) + "\n")

        payload = make_payload({
            "agent_id": agent_id,
            "last_assistant_message": f"Read {brief}. Task: BUG-ogblk",
            "agent_type": "deepseek-v4-flash",
        })
        rc, stdout, stderr = run_hook(payload, {
            "GATE_RUNNER_VERDICTS_DIR": verdicts_dir,
        })

        if rc == 0:
            ok("T5: exit 0 (allowed after MAX_BLOCKS)")
        else:
            fail("T5: exit 0", f"got {rc} (should not block at MAX_BLOCKS)")

        if "systemMessage" in stdout:
            ok("T5: systemMessage in output")
        else:
            fail("T5: systemMessage", "stdout: " + stdout[:200])


def test_6_stop_hook_active():
    """stop_hook_active=True → exit 0 immediately, no gate run."""
    clean_state()
    payload = make_payload({
        "stop_hook_active": True,
        "agent_type": "deepseek-v4-flash",
    })
    rc, stdout, stderr = run_hook(payload)

    if rc == 0:
        ok("T6: exit 0 (stop_hook_active)")
    else:
        fail("T6: exit 0", f"got {rc}")

    if stdout == "":
        ok("T6: no output (immediate return)")
    else:
        fail("T6: no output", f"stdout: {stdout}")


def main():
    print("=== subagent-stop-gate tests ===")
    print()

    test_1_executor_pass()
    print()
    test_2_executor_fail()
    print()
    test_3_unknown_shape()
    print()
    test_4_non_executor()
    print()
    test_5_max_blocks()
    print()
    test_6_stop_hook_active()
    print()

    print(f"=== Results: {PASSED} passed, {FAILED} failed ===")
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
