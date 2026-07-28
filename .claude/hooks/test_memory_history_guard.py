#!/usr/bin/env python3
"""Tests for memory-history-guard.py — run: python3 test_memory_history_guard.py"""
import json
import subprocess
import sys
from pathlib import Path

HOOK = Path(__file__).with_name("memory-history-guard.py")
MEM = "/Users/x/.claude/projects/-Users-x-Proj/memory"


def run(payload):
    p = subprocess.run(
        [sys.executable, str(HOOK)], input=json.dumps(payload),
        capture_output=True, text=True,
    )
    assert p.returncode == 0, p.stderr
    return json.loads(p.stdout) if p.stdout.strip() else None


def denied(out):
    return bool(out) and out["hookSpecificOutput"]["permissionDecision"] == "deny"


cases = [
    # (name, payload, expect_deny)
    ("write with hash denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "content": "fixed at ef59f615"}}, True),
    ("write with status denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/handoff_y.md",
                       "content": "P1 SHIPPED same day"}}, True),
    ("clean write allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/feedback_z.md",
                       "content": "Never do the thing; done via the other thing."}}, False),
    ("decision log exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/guide_decision_log.md",
                       "content": "SHIPPED 2026-06-07 @ deadbee1"}}, False),
    ("non-memory path ignored", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/A.md",
                       "content": "LANDED @ deadbee1"}}, False),
    ("edit adding hash denied", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "old_string": "old text",
                       "new_string": "landed 74563a4c"}}, True),
    ("edit carrying existing hash forward allowed", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "old_string": "landed 74563a4c foo",
                       "new_string": "landed 74563a4c bar"}}, False),
    ("lowercase done not a status stamp", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "content": "analysis is done per-send, pay-per-use"}}, False),
    ("hex-alphabet word without digit allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "content": "the decoder accedes to the facade"}}, False),
]

failures = 0
for name, payload, expect in cases:
    got = denied(run(payload))
    if got != expect:
        print(f"FAIL: {name} (expected deny={expect}, got deny={got})")
        failures += 1
print(f"{len(cases) - failures}/{len(cases)} passed")
sys.exit(1 if failures else 0)
