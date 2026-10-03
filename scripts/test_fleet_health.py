#!/usr/bin/env python3
"""Tests for scripts/fleet_health.py: blocker detection and slot naming."""

import sys
import unittest
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


class SlotNameTests(unittest.TestCase):
    def test_slot_one_does_not_match_slot_ten(self):
        self.assertFalse(fleet_health.slot_named("slot-1", "work in .claude/worktrees/slot-10/"))
        self.assertTrue(fleet_health.slot_named("slot-1", "work in .claude/worktrees/slot-1/"))
        self.assertTrue(fleet_health.slot_named("slot-1", "in slot-1 (branch x)"))


if __name__ == "__main__":
    unittest.main()
