#!/usr/bin/env python3
"""Append a new entry to docs/BUG_BACKLOG.md — the one tool for logging a bug.

Exists because agents kept re-deriving the format each session (next free ID,
index-row shape, ``**Status:**`` line, Symptom/Root cause/Fix shape structure)
by reading old entries and guessing — slow and inconsistent. This tool mints
the ID, writes the entry in the standard shape, and inserts the index row, in
one call. Pairs with ``bug_status.py``, which only checks/reflows; this is the
one that *creates*.

Usage:
    python3 .claude/hooks/log_bug.py \\
        --slug some-kebab-slug \\
        --title "Short title after the id/slug" \\
        --severity MED \\
        --symptom "What a performer/user would observe." \\
        --fix-shape "What the fix looks like." \\
        [--root-cause "Known cause, or 'unknown — <suspects>'"] \\
        [--context "CINEMATIC_POST P1 session"]   # appended as ", found not fixed <date> (<context>)"
        [--status OPEN]                            # default OPEN
        [--id BUG-NNN]                              # override auto-assigned id
        [--dry-run]                                 # print the diff, don't write

Refuses to write in the main checkout (landing protocol: edits go through a
worktree) unless --force — same guard as bug_status.py --write.
"""
from __future__ import annotations

import argparse
import re
import sys
from datetime import date
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
REPO = HOOKS_DIR.parents[1]
BACKLOG = REPO / "docs" / "BUG_BACKLOG.md"

sys.path.insert(0, str(HOOKS_DIR))
import bug_status  # noqa: E402


def next_id(head_lines: list[str], entries: list["bug_status.Entry"]) -> str:
    nums = set()
    for bug_id in bug_status.index_ids(head_lines):
        m = re.match(r"BUG-(\d+)$", bug_id)
        if m:
            nums.add(int(m.group(1)))
    for e in entries:
        m = re.match(r"BUG-(\d+)$", e.id)
        if m:
            nums.add(int(m.group(1)))
    for bug_id in bug_status.archive_ids():
        m = re.match(r"BUG-(\d+)$", bug_id)
        if m:
            nums.add(int(m.group(1)))
    return f"BUG-{(max(nums) + 1) if nums else 1}"


def build_entry(bug_id: str, args) -> list[str]:
    severity = args.severity
    if args.context:
        severity = f"{severity}, found not fixed {date.today().isoformat()} ({args.context})"
    lines = [
        f"### {bug_id} ({args.slug}) — {args.title} — {severity}",
        f"**Status:** {args.status}",
        "",
        f"**Symptom:** {args.symptom}",
    ]
    if args.root_cause:
        lines.append(f"**Root cause:** {args.root_cause}")
    lines.append(f"**Fix shape:** {args.fix_shape}")
    return lines


def build_index_row(bug_id: str, args) -> str:
    parts = [args.symptom]
    if args.root_cause:
        parts.append(f"Root cause: {args.root_cause}")
    parts.append(f"Fix shape: {args.fix_shape}")
    one_line = " ".join(parts) + f" {args.severity}."
    return f"| {bug_id} | **{args.slug}** | {one_line} |"


def insert(text: str, bug_id: str, args) -> str:
    """Splice the new index row + entry directly into the raw lines.

    Deliberately does NOT go through ``bug_status.rebuild()`` — that helper
    reconstructs the file from parsed ``Entry`` objects (plus, since the
    BUG-139 fix, the ## Fixed archive-pointer lines) and still drops any
    other "stray" line. Splicing preserves everything untouched except the
    two insertion points.
    """
    lines = text.split("\n")

    row = build_index_row(bug_id, args)
    sep_idx = next(i for i, l in enumerate(lines) if l.strip().startswith("|---"))
    lines = lines[: sep_idx + 1] + [row] + lines[sep_idx + 1 :]

    fixed_idx = next(i for i, l in enumerate(lines) if l.strip() == "## Fixed")
    end = fixed_idx
    while end > 0 and lines[end - 1].strip() == "":
        end -= 1
    entry_lines = build_entry(bug_id, args)
    lines = lines[:end] + [""] + entry_lines + [""] + lines[end:]

    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--slug", required=True, help="kebab-case nickname, e.g. some-bug-slug")
    p.add_argument("--title", required=True, help="title after the id/slug in the ### heading")
    p.add_argument("--severity", required=True, help="LOW / MED / MED-HIGH / HIGH (free text)")
    p.add_argument("--symptom", required=True)
    p.add_argument("--fix-shape", required=True, dest="fix_shape")
    p.add_argument("--root-cause", dest="root_cause", default="")
    p.add_argument("--context", default="", help='e.g. "CINEMATIC_POST P1 session" — appended to severity')
    p.add_argument("--status", default="OPEN", choices=sorted(bug_status.ALL_STATUSES))
    p.add_argument("--id", dest="bug_id", default="", help="override auto-assigned BUG-NNN")
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--force", action="store_true", help="allow writing in the main checkout")
    args = p.parse_args()

    if (REPO / ".git").is_dir() and not args.dry_run and not args.force:
        raise SystemExit(
            "refusing to write in the MAIN checkout (landing protocol: edits go "
            "through a worktree). Run the worktree's copy instead:\n"
            "  python3 .claude/worktrees/<name>/.claude/hooks/log_bug.py ...\n"
            "or pass --dry-run to preview, or --force to override deliberately.")

    text = BACKLOG.read_text()
    head, entries, _, _, _ = bug_status.parse(text)
    bug_id = args.bug_id or next_id(head, entries)
    if any(e.id == bug_id for e in entries) or bug_id in bug_status.index_ids(head):
        raise SystemExit(f"{bug_id} already exists — pick a different --id or omit it to auto-assign")

    new_text = insert(text, bug_id, args)

    problems = bug_status.check(new_text)
    if problems:
        print("warning: bug-backlog status drift after insert (check manually):", file=sys.stderr)
        for prob in problems:
            print(f"  · {prob}", file=sys.stderr)

    if args.dry_run:
        print(f"would assign {bug_id}, writing:\n")
        print("\n".join(build_entry(bug_id, args)))
        print()
        print(build_index_row(bug_id, args))
        return 0

    BACKLOG.write_text(new_text)
    print(f"logged {bug_id} ({args.slug}) to {BACKLOG.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
