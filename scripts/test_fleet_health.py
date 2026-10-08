#!/usr/bin/env python3
"""Tests for scripts/fleet_health.py: blocker detection and slot naming."""

import json
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fleet_health  # noqa: E402


class BlockerTests(unittest.TestCase):
    def test_final_output_is_after_the_last_marker(self):
        log = "started\nNo Metal device in an earlier draft\nFinal output\nAll checks passed."
        self.assertEqual(fleet_health.blockers_in(fleet_health.final_output(log)), [])

    def test_known_blockers_are_named(self):
        cases = {
            "panicked: No Metal device found": "no Metal device",
            "the disk has 44 GiB free and requires 50 GiB": "disk guard",
            "write failed: Operation not permitted": "sandbox refusal",
            "I am blocked on the lead's GPU run": "says blocked",
        }
        for text, name in cases.items():
            self.assertIn(name, fleet_health.blockers_in("Final output\n" + text), text)

    def test_routine_commit_note_is_not_a_blocker(self):
        text = "Final output\nNo git writes; the lead commits (index.lock is outside my sandbox)."
        self.assertEqual(fleet_health.blockers_in(text), [])

    def test_running_admission_blocker_repeats_until_explicit_resolution(self):
        blocked = "Cargo blocked before compilation: storage admission reported 48 GiB free."
        self.assertEqual(fleet_health.running_blockers(blocked), ["disk admission"])
        self.assertEqual(fleet_health.running_blockers(
            blocked + "\nCommand completed: cargo check (exit 0)"),
            ["disk admission"])
        self.assertEqual(fleet_health.running_blockers(
            blocked + "\nStorage admission allowed the next command."), [])
        self.assertEqual(fleet_health.running_blockers(
            blocked + "\nCommand completed: cargo check (exit 0)\n"),
            ["disk admission"])

    def test_running_approval_refusal_is_actionable(self):
        text = "Automatic approval review blocked the requested command."
        self.assertEqual(fleet_health.running_blockers(text), ["approval-review refusal"])

    def test_prompt_instruction_does_not_count_as_running_blocker(self):
        text = "If approval review refuses this command, stop and report it."
        self.assertEqual(fleet_health.running_blockers(text), [])

    def test_running_job_report_repeats_until_resolved(self):
        with tempfile.TemporaryDirectory() as d:
            log = Path(d) / 'job.log'
            log.write_text('Automatic approval review rejected the command.')
            job = {'id': 'lane', 'status': 'running', 'pid': 123, 'logFile': str(log)}
            seen = {'lane': 'running'}
            with patch.object(fleet_health, 'pid_alive', return_value=True), \
                    patch.object(fleet_health, 'session_file', return_value=None):
                for _ in range(2):
                    problems = []
                    fleet_health.check_jobs([job], seen, log.stat().st_mtime, problems)
                    self.assertTrue(any('running but blocked' in line for _, line in problems))
                log.write_text(log.read_text() + '\nApproval review resolved by lead.')
                problems = []
                fleet_health.check_jobs([job], seen, log.stat().st_mtime, problems)
                self.assertFalse(any('running but blocked' in line for _, line in problems))

    def test_session_reader_excludes_prompt_and_reads_tool_outputs(self):
        records = [
            {"type": "event_msg", "payload": {"item": {
                "type": "UserMessage", "content": "approval review blocked"}}},
            {"type": "response_item", "payload": {
                "type": "function_call_output",
                "output": "Automatic approval review blocked the command.",
            }},
            {"type": "event_msg", "payload": {"item": {
                "type": "CommandExecution", "stdout": "approval review resolved by lead"}}},
        ]
        with tempfile.NamedTemporaryFile(mode="w+", suffix=".jsonl") as stream:
            stream.write("\n".join(json.dumps(record) for record in records))
            stream.flush()
            evidence = fleet_health._session_evidence(Path(stream.name))
        self.assertNotIn('"content": "approval review blocked"', evidence)
        self.assertIn("Automatic approval review blocked", evidence)
        self.assertIn("approval review resolved", evidence)


class SlotNameTests(unittest.TestCase):
    def test_slot_one_does_not_match_slot_ten(self):
        self.assertFalse(fleet_health.slot_named("slot-1", "work in .claude/worktrees/slot-10/"))
        self.assertTrue(fleet_health.slot_named("slot-1", "work in .claude/worktrees/slot-1/"))
        self.assertTrue(fleet_health.slot_named("slot-1", "in slot-1 (branch x)"))


if __name__ == "__main__":
    unittest.main()
