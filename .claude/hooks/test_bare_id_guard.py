#!/usr/bin/env python3
"""Tests for bare-id-guard.py — run: python3 test_bare_id_guard.py"""
import json
import subprocess
import sys
from pathlib import Path

HOOK = Path(__file__).with_name("bare-id-guard.py")
DOCS = "/Users/x/Proj/docs"
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
    ("bare id in docs denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "See BUG-lu32 for details."}}, True),
    ("paren-named id allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "See BUG-lu32 (phantom-clip double-commit)."}}, False),
    ("em-dash-named id allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "BUG-297 — multi-session memory exhaustion"}}, False),
    ("one naming legitimises later bare mentions", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "BUG-lu32 (phantom-clip double-commit) is open.\n"
                                  "BUG-lu32 blocks the release."}}, False),
    ("backticked id with name allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "Track it as `BUG-0di` (memory exhaustion freeze)."}}, False),
    ("bd command line exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "Run bd show BUG-lu32 to inspect."}}, False),
    ("code fence exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "Example:\n```\nBUG-lu32\n```\ndone."}}, False),
    ("external_ref line exempt", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "content": "external_ref: BUG-219"}}, False),
    ("memory dir in scope", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{MEM}/project_x.md",
                       "content": "waiting on BUG-4jv"}}, True),
    ("non-md file ignored", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/src/main.rs",
                       "content": "// BUG-lu32"}}, False),
    ("md outside docs/memory/CLAUDE.md ignored", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/README.md",
                       "content": "BUG-lu32"}}, False),
    ("edit adding bare id denied", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "old_string": "old text",
                       "new_string": "blocked by BUG-62l3"}}, True),
    ("edit carrying bare id forward allowed", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "old_string": "blocked by BUG-62l3 today",
                       "new_string": "blocked by BUG-62l3 still"}}, False),
    ("edit bare id named in old_string allowed", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "old_string": "BUG-62l3 (confused-deputy dispatch) intro",
                       "new_string": "BUG-62l3 (confused-deputy dispatch) intro\n"
                                     "BUG-62l3 remains open."}}, False),
    ("bare cross-doc secref denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "See docs/WIDGET_TREE_DESIGN.md section 5b for the recipe."}}, True),
    ("named cross-doc secref allowed", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "See docs/WIDGET_TREE_DESIGN.md section 5b (param-surface recipe)."}}, False),
    ("same-doc secref stays bare", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "Details in section 5b below."}}, False),
    ("banned section symbol denied", {
        "tool_name": "Write",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "content": "See \u00a75b for the recipe."}}, True),
    ("symbol carried forward in edit allowed", {
        "tool_name": "Edit",
        "tool_input": {"file_path": f"{DOCS}/A.md",
                       "old_string": "see \u00a72 here",
                       "new_string": "see \u00a72 there"}}, False),
    ("CLAUDE.md in scope", {
        "tool_name": "Write",
        "tool_input": {"file_path": "/Users/x/Proj/CLAUDE.md",
                       "content": "see BUG-lu32"}}, True),
]

failures = 0
for name, payload, expect in cases:
    got = denied(run(payload))
    if got != expect:
        print(f"FAIL: {name} (expected deny={expect}, got deny={got})")
        failures += 1
print(f"{len(cases) - failures}/{len(cases)} passed")
sys.exit(1 if failures else 0)
