#!/usr/bin/env python3
"""Tests for bug-backlog-tool-guard.py — the PreToolUse guard that forces new
BUG entries / index rows / status changes through log_bug.py & bug_status.py,
while leaving free-text body edits hand-editable.

Run: python3 .claude/hooks/test_bug_backlog_tool_guard.py
"""
import json
import subprocess
from pathlib import Path

HOOK = Path(__file__).resolve().parent / "bug-backlog-tool-guard.py"
BL = "/tmp/some-checkout/docs/BUG_BACKLOG.md"      # any path named BUG_BACKLOG.md
ARCHIVE = "/tmp/some-checkout/docs/BUG_BACKLOG_CLOSED.md"
OTHER = "/tmp/some-checkout/docs/NODE_CATALOG.md"

PASS = []
FAIL = []


def decision(payload):
    r = subprocess.run(["python3", str(HOOK)], input=json.dumps(payload),
                       capture_output=True, text=True)
    out = r.stdout.strip()
    if not out:
        return "allow"
    return json.loads(out)["hookSpecificOutput"]["permissionDecision"]


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    if not cond:
        print(f"FAIL: {name} {detail}")


def edit(fp, old, new):
    return {"tool_name": "Edit", "tool_input": {"file_path": fp, "old_string": old, "new_string": new}}


def write(fp, content):
    return {"tool_name": "Write", "tool_input": {"file_path": fp, "content": content}}


# DENY: structured mutations.
check("new entry heading denied",
      decision(edit(BL, "## Fixed\n", "### BUG-329 (x) — y — MED\n**Status:** OPEN\n## Fixed\n")) == "deny")
check("new index row denied",
      decision(edit(BL, "|---|---|---|\n", "|---|---|---|\n| BUG-329 | **x** | z MED. |\n")) == "deny")
check("status flip denied",
      decision(edit(BL, "**Status:** OPEN", "**Status:** FIXED @ abc")) == "deny")
check("status newly added denied",
      decision(edit(BL, "### BUG-5 (x) — y — MED\n**Symptom:** a",
                    "### BUG-5 (x) — y — MED\n**Status:** OPEN\n**Symptom:** a")) == "deny")
check("whole-file Write denied", decision(write(BL, "# Bug backlog\n### BUG-1\n")) == "deny")

# ALLOW: body prose, unchanged structural context, other files.
check("root-cause addendum allowed",
      decision(edit(BL, "**Symptom:** it breaks",
                    "**Symptom:** it breaks\n**Root cause:** foo does bar")) == "allow")
check("heading present-in-both (context) allowed",
      decision(edit(BL, "### BUG-300 (x) — y — MED\n**Symptom:** a",
                    "### BUG-300 (x) — y — MED\n**Symptom:** a longer")) == "allow")
check("status present-in-both unchanged allowed",
      decision(edit(BL, "**Status:** OPEN\n**Symptom:** a",
                    "**Status:** OPEN\n**Symptom:** a much longer symptom")) == "allow")
check("archive file not guarded",
      decision(edit(ARCHIVE, "x", "### BUG-329 (x) — y — MED")) == "allow")
check("non-backlog file not guarded",
      decision(edit(OTHER, "x", "### BUG-329 (x) — y — MED\n**Status:** OPEN")) == "allow")
check("non-edit tool ignored",
      decision({"tool_name": "Bash", "tool_input": {"command": "ls"}}) == "allow")

print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
raise SystemExit(1 if FAIL else 0)
