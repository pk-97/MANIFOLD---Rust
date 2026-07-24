#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) guard: the STRUCTURED parts of
docs/BUG_BACKLOG.md go through the tools, never a hand edit.

Why this exists: agents hand-type new ``### BUG-NNN`` entries, index rows, and
``**Status:`` flips straight into the markdown. That (a) re-derives the format
by eye — misplaced sections, wrong index shape, wordy id bookkeeping — and, worse,
(b) picks the id by scanning the LOCAL checkout, so two concurrent worktrees both
mint the same number (a real duplicate BUG-328 already sits on two branches). The
tools remove both: ``log_bug.py`` mints a collision-safe id from a shared,
flock-guarded counter and writes the entry + index row in one shot; ``bug_status.py
--write`` is the single source of truth for status + section placement.

Denies exactly the structured mutations, on the target file ``BUG_BACKLOG.md`` in
ANY checkout (main or a worktree — agents hand-edit in worktrees too):
  - a NEW ``### BUG-NNN`` entry heading (present in new text, absent in old);
  - a NEW index-table row for a BUG id (present in new, absent in old);
  - a CHANGED/added ``**Status:`` line value.
Write (a full-file rewrite) is always denied — never the right way to touch this
file.

Deliberately does NOT block free-text body edits — adding a ``**Root cause:``
addendum, refining ``**Fix shape:``, appending investigation history. Those are
real prose and stay hand-editable; only id / index-row / status structure is gated.
(So this guard would NOT have stopped the slot-9 proof-note deletion — that is a
body edit / judgment problem, not a formatting one.)

Carve-out: during an in-progress merge in the target's checkout (``MERGE_HEAD``
present), conflict resolution legitimately hand-edits status lines in the merged
BUG_BACKLOG — the guard falls silent, mirroring worktree-guard's merge exemption.

The tools write via Python file I/O (a Bash call), not the Edit/Write tool, so they
are never matched by this hook — no self-block.

Fails OPEN on any error or unrecognized shape. Receives
`{"tool_name", "tool_input": {...}, "cwd"}` on stdin; emits
hookSpecificOutput.permissionDecision="deny" + reason, or nothing.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

TARGET = "BUG_BACKLOG.md"  # NOT the archive (BUG_BACKLOG_CLOSED.md)

HEADING = re.compile(r"^\s*###\s+BUG-\d+", re.M)
INDEX_ROW = re.compile(r"^\s*\|\s*~*\s*BUG-(\d+)", re.M)
STATUS = re.compile(r"^\s*\*\*Status:\*\*\s*(.+?)\s*$", re.M)

LOG_BUG_MSG = (
    "Blocked: don't hand-edit `docs/BUG_BACKLOG.md` structure. This edit adds a "
    "new BUG entry/index row. Use the tool — it mints a collision-safe id from a "
    "shared counter (two worktrees can't grab the same number) and writes the "
    "entry + index row + self-check in one call:\n\n"
    "  python3 .claude/hooks/log_bug.py --slug <kebab> --title \"...\" "
    "--severity MED --symptom \"...\" --fix-shape \"...\" [--root-cause \"...\"]\n\n"
    "Run it from your worktree's copy (it refuses the main checkout by design). "
    "Editing an existing entry's prose body is fine — only new ids/rows are gated."
)
STATUS_MSG = (
    "Blocked: don't hand-edit a `**Status:` line in `docs/BUG_BACKLOG.md`. The "
    "status line is the single source of truth for open/fixed and section "
    "placement — set it and reflow with the tool:\n\n"
    "  python3 .claude/hooks/bug_status.py --write   (run from your worktree)\n\n"
    "Editing the bug's prose body (root cause, fix shape, history) is fine."
)
WRITE_MSG = (
    "Blocked: don't rewrite `docs/BUG_BACKLOG.md` with Write. Log a bug with "
    "`log_bug.py`, change status/section with `bug_status.py --write`. Body prose "
    "edits go through Edit, not a whole-file Write."
)


def extract_old_new(tool_name, tool_input):
    if tool_name == "Edit":
        return tool_input.get("old_string", ""), tool_input.get("new_string", "")
    if tool_name == "MultiEdit":
        edits = tool_input.get("edits") or []
        return (
            "\n".join(e.get("old_string", "") for e in edits),
            "\n".join(e.get("new_string", "") for e in edits),
        )
    if tool_name == "Write":
        return None, tool_input.get("content", "")
    return None, None


def merge_in_progress(resolved: Path) -> bool:
    try:
        r = subprocess.run(
            ["git", "-C", str(resolved.parent), "rev-parse", "-q", "--verify", "MERGE_HEAD"],
            capture_output=True, text=True, timeout=5,
        )
        return r.returncode == 0
    except Exception:
        return False


def deny(reason: str):
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    tool_name = data.get("tool_name")
    if tool_name not in ("Edit", "Write", "MultiEdit"):
        return 0

    tool_input = data.get("tool_input") or {}
    file_path = tool_input.get("file_path") or ""
    if not file_path:
        return 0
    try:
        resolved = Path(file_path)
        if not resolved.is_absolute():
            resolved = (Path(data.get("cwd") or ".") / resolved)
        resolved = resolved.resolve()
    except Exception:
        return 0

    if resolved.name != TARGET:
        return 0
    if merge_in_progress(resolved):
        return 0

    old, new = extract_old_new(tool_name, tool_input)
    if new is None:
        return 0

    if old is None:  # Write — whole-file rewrite
        deny(WRITE_MSG)
        return 0

    # New entry heading, or a new index row for an id not previously present.
    added_heading = HEADING.search(new) and not HEADING.search(old)
    new_ids = set(INDEX_ROW.findall(new)) - set(INDEX_ROW.findall(old))
    if added_heading or new_ids:
        deny(LOG_BUG_MSG)
        return 0

    # A status value that isn't in the old text (changed or newly added).
    if set(s.strip() for s in STATUS.findall(new)) - set(s.strip() for s in STATUS.findall(old)):
        deny(STATUS_MSG)
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
