#!/usr/bin/env python3
"""Tests for permission-request-guard.py, run as a standalone subprocess.

Worker payloads (agent_id / teammate_name / agent_type) get a deny with the reroute
note. Lead payloads get no output, so the prompt proceeds. Bad or empty stdin fails
open with no output and exit 0.
"""
import json
import subprocess
import sys
import unittest
from pathlib import Path

GUARD = Path(__file__).resolve().parent / "permission-request-guard.py"


def run(stdin_text):
    p = subprocess.run([sys.executable, str(GUARD)], input=stdin_text,
                       capture_output=True, text=True)
    return p.returncode, p.stdout.strip()


def pr_payload(cmd, worker, tool="Bash"):
    p = {"hook_event_name": "PermissionRequest", "tool_name": tool,
         "tool_input": {"command": cmd} if tool == "Bash" else {"file_path": cmd},
         "permission_mode": "auto", "session_id": "t", "cwd": "/tmp",
         "tool_use_id": "toolu_test",
         "permission_suggestions": [{"behavior": "allow", "rule": "Bash(x *)"}]}
    if worker:
        p["agent_id"] = "agent-test-1"
        p["agent_type"] = "lane"
    return p


class PermissionRequestGuardTest(unittest.TestCase):
    def test_worker_bash_prompt_is_denied_with_reroute(self):
        code, out = run(json.dumps(pr_payload("sed -n 'w /tmp/x' f", worker=True)))
        self.assertEqual(code, 0)
        d = json.loads(out)["hookSpecificOutput"]["decision"]
        self.assertEqual(d["behavior"], "deny")
        self.assertIn("WORKER SEAT", d["message"])
        self.assertIn("report", d["message"])

    def test_worker_edit_prompt_is_denied_too(self):
        code, out = run(json.dumps(pr_payload("x", worker=True, tool="Edit")))
        self.assertEqual(json.loads(out)["hookSpecificOutput"]["decision"]["behavior"], "deny")

    def test_teammate_name_counts_as_worker(self):
        p = pr_payload("x", worker=False)
        p["teammate_name"] = "sonnet-lane-3"
        code, out = run(json.dumps(p))
        self.assertEqual(json.loads(out)["hookSpecificOutput"]["decision"]["behavior"], "deny")

    def test_lead_prompt_gets_no_verdict(self):
        code, out = run(json.dumps(pr_payload("sed -n 'w /tmp/x' f", worker=False)))
        self.assertEqual(code, 0)
        self.assertEqual(out, "")

    def test_garbage_stdin_fails_open(self):
        code, out = run("not json")
        self.assertEqual(code, 0)
        self.assertEqual(out, "")

    def test_empty_stdin_fails_open(self):
        code, out = run("")
        self.assertEqual(code, 0)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
