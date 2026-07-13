#!/usr/bin/env python3
"""One-shot self-grade logger for daemon fires (RUNBOOK step 2, session side).

Sessions used to re-derive the record format from RUNBOOK.md on every fire —
this script owns the format instead. One line, no reasoning required:

    python3 .claude/daemon/log_grade.py <seq> <move_id> <correct> <effective> "<notes>"

    python3 .claude/daemon/log_grade.py 3 mechanical/git-landing n n \\
        "Misfire: fired on a read-only rg; the deletion already ran is-ancestor."

`correct` / `effective` accept y/n/true/false plus the canonical specials
(`miss`/`unclear` for correct, `unclear`/`na` for effective). Values are
normalized to the canonical vocabulary and linted with eval/check_grades.py
before the line is written, so a malformed grade fails loudly here instead of
corrupting the corpus silently.

session_id comes from $CLAUDE_CODE_SESSION_ID (override: --session-id).
Worker nudges MUST pass --agent-id (RUNBOOK step 2: (session_id, seq) alone
collides across workers). The record always lands in the MAIN checkout's
eval/live_grades.session.jsonl — even when this script runs from a worktree
copy — because the sleep pass only reads the main checkout's session files.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR / "eval"))
import check_grades  # noqa: E402  — single source of truth for the vocabulary

CORRECT_MAP = {"y": True, "yes": True, "true": True, "tp": True,
               "n": False, "no": False, "false": False, "fp": False,
               "miss": "miss", "unclear": "unclear"}
EFFECTIVE_MAP = {"y": True, "yes": True, "true": True,
                 "n": False, "no": False, "false": False,
                 "unclear": "unclear", "na": "n/a", "n/a": "n/a"}


def main_daemon_dir() -> Path:
    """The MAIN checkout's .claude/daemon, even when run from a worktree copy."""
    try:
        common = subprocess.run(
            ["git", "-C", str(SCRIPT_DIR), "rev-parse", "--path-format=absolute",
             "--git-common-dir"],
            capture_output=True, text=True, check=True).stdout.strip()
        return Path(common).parent / ".claude" / "daemon"
    except Exception:
        return SCRIPT_DIR


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("seq", type=int, help="the fire's seq, named in the daemon note")
    p.add_argument("move_id", help="e.g. mechanical/git-landing")
    p.add_argument("correct", choices=sorted(CORRECT_MAP),
                   help="did the named drift actually exist?")
    p.add_argument("effective", choices=sorted(EFFECTIVE_MAP),
                   help="did behavior change in the payload's direction?")
    p.add_argument("notes", nargs="?", default="",
                   help="one sentence of evidence (quoted)")
    p.add_argument("--agent-id", default="", help="REQUIRED for worker-nudge fires")
    p.add_argument("--session-id", default=os.environ.get("CLAUDE_CODE_SESSION_ID", ""))
    p.add_argument("--file", default="", help="override target (testing only)")
    args = p.parse_args()

    if not args.session_id:
        raise SystemExit("no session id: $CLAUDE_CODE_SESSION_ID unset — pass --session-id")

    correct = CORRECT_MAP[args.correct]
    effective = EFFECTIVE_MAP[args.effective]
    if effective == "n/a" and correct != "miss":
        raise SystemExit('effective "na" is only meaningful on a correct "miss" record')
    assert correct in check_grades.CORRECT_OK and effective in check_grades.EFFECTIVE_OK

    rec = {"ts": datetime.date.today().isoformat(), "session_id": args.session_id,
           "seq": args.seq, "move_id": args.move_id, "correct": correct,
           "effective": effective, "grader": "session", "notes": args.notes}
    if args.agent_id:
        rec["agent_id"] = args.agent_id

    target = Path(args.file) if args.file else (
        main_daemon_dir() / "eval" / "live_grades.session.jsonl")
    with open(target, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")

    problems = check_grades.lint(target)
    if problems:
        for pr in problems:
            print(pr, file=sys.stderr)
        raise SystemExit(f"appended, but {target.name} now fails check_grades — fix it")
    print(f"logged seq {args.seq} ({args.move_id}) -> {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
