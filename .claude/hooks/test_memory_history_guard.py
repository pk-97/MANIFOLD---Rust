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
    # Attribution and dated provenance in rule text.
    ("memory 'Peter said' denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/feedback_x.md",
                       "content": "Peter said never pin effort low."}}, True),
    ("CLAUDE.md dated attribution denied", {
        "tool_name": "Edit",
        "tool_input": {"file_path": "/Users/x/Proj/CLAUDE.md",
                       "old_string": "Agents do the work.",
                       "new_string": "Agents do the work (Peter, 2026-10-13)."}}, True),
    ("docs 'per Peter' denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/AGENT_ROUTING.md",
                       "content": "Effort is medium per Peter."}}, True),
    ("docs 'Peter approved <date>' denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/AGENT_ROUTING.md",
                       "content": "Land with land_branch (Peter approved 2026-07-31)."}}, True),
    ("docs 'Peter ruled' denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/AGENT_ROUTING.md",
                       "content": "Peter ruled that gates are the gates."}}, True),
    (".claude nudge 'Peter 2026-07-29' denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/.claude/hooks/context-nudges/x.md",
                       "content": "Perf soak is not a gate (Peter 2026-07-29)."}}, True),
    ("conditional 'if Peter says go' allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/A.md",
                       "content": "If Peter says go, go; land only on a branch Peter approved."}}, False),
    ("Peter as a subject of a rule allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/AGENT_ROUTING.md",
                       "content": "Peter's eye is a stage check after landing."}}, False),
    ("dated event line without attribution allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/CLAUDE.md",
                       "content": "Hand-rolled worktrees filled the disk (455 GB, 2026-07-15)."}}, False),
    ("existing attribution carried forward allowed", {
        "tool_name": "Edit",
        "tool_input": {"file_path": "/Users/x/Proj/docs/A.md",
                       "old_string": "rule one (Peter, 2026-10-01) foo",
                       "new_string": "rule one (Peter, 2026-10-01) bar"}}, False),
    ("second attribution beside an old one denied", {
        "tool_name": "Edit",
        "tool_input": {"file_path": "/Users/x/Proj/docs/A.md",
                       "old_string": "rule one (Peter, 2026-10-01)",
                       "new_string": "rule one (Peter, 2026-10-01), rule two (Peter, 2026-10-01)"}}, True),
    ("decision log attribution exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/guide_decision_log.md",
                       "content": "Peter ruled 2026-10-14: effort medium."}}, False),
    ("design-doc D-entry attribution exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/FOO_DESIGN.md",
                       "content": "- **D-12** Loop wraps at the bar (Peter, 2026-09-01)."}}, False),
    ("design-doc prose attribution denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/docs/FOO_DESIGN.md",
                       "content": "The loop wraps at the bar (Peter, 2026-09-01)."}}, True),
    ("non-markdown file ignored", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/scripts/x.py",
                       "content": "# Peter said so (Peter, 2026-10-01)"}}, False),
]

failures = 0
for name, payload, expect in cases:
    got = denied(run(payload))
    if got != expect:
        print(f"FAIL: {name} (expected deny={expect}, got deny={got})")
        failures += 1
print(f"{len(cases) - failures}/{len(cases)} passed")
sys.exit(1 if failures else 0)
