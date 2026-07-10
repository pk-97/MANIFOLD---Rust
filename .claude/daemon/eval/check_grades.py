#!/usr/bin/env python3
"""Schema lint for the grade corpora (RUNBOOK step 2's canonical-value rule,
made deterministic). Pass-1 lesson: free-text grades ("TP", "y") silently
corrupt precision math downstream — this catches them at write time instead
of at analysis time. Run after any grading session, and as the first QC step
when reviewing another model's grading output.

Usage: python3 check_grades.py [file.jsonl ...]
Default files: live_grades.jsonl + any live_grades.session*.jsonl beside it.
Exit 0 = clean; exit 1 with one line per violation.
"""
import glob
import json
import os
import sys

# Canonical grade vocabulary (RUNBOOK step 2). Extended 2026-07-10 (Opus sleep
# pass 2) to be symmetric and honest about the two undeterminable cases the
# corpus actually contains:
#   correct: "unclear" — the drift's *existence* could not be determined from
#     the available evidence (worker fires whose move_id/window is lost to
#     mailbox overwrite or colliding attribution; redelivery suspects). Parallels
#     effective:"unclear"; excluded from precision denominators, never guessed.
#   effective: "n/a" — meaningful ONLY on a correct:"miss" record (a false
#     negative). No payload was delivered, so "did behavior move in the payload's
#     direction" is undefined, not merely unknown.
CORRECT_OK = (True, False, "miss", "unclear")
EFFECTIVE_OK = (True, False, "unclear", "n/a")
REQUIRED = ("ts", "session_id", "move_id", "correct", "effective")


def lint(path):
    problems = []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError as e:
                problems.append(f"{path}:{n}: unparseable JSON ({e})")
                continue
            for key in REQUIRED:
                if key not in rec:
                    problems.append(f"{path}:{n}: missing required key '{key}'")
            if "correct" in rec and rec["correct"] not in CORRECT_OK:
                problems.append(
                    f"{path}:{n}: non-canonical correct={rec['correct']!r} "
                    f"(must be true/false/\"miss\")"
                )
            if "effective" in rec and rec["effective"] not in EFFECTIVE_OK:
                problems.append(
                    f"{path}:{n}: non-canonical effective={rec['effective']!r} "
                    f"(must be true/false/\"unclear\")"
                )
            if "seq" not in rec:
                problems.append(f"{path}:{n}: missing 'seq' (null is fine, absent is not)")
    return problems


def main(argv):
    files = argv[1:]
    if not files:
        here = os.path.dirname(os.path.abspath(__file__))
        files = [os.path.join(here, "live_grades.jsonl")]
        files += sorted(glob.glob(os.path.join(here, "live_grades.session*.jsonl")))
        files = [p for p in files if os.path.exists(p)]
    all_problems = []
    total = 0
    for path in files:
        total += sum(1 for line in open(path, encoding="utf-8") if line.strip())
        all_problems.extend(lint(path))
    for p in all_problems:
        print(p)
    print(f"{'FAIL' if all_problems else 'OK'}: {total} records across {len(files)} file(s), "
          f"{len(all_problems)} violation(s)")
    return 1 if all_problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
