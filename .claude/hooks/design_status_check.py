#!/usr/bin/env python3
"""Design-status housekeeper — runs on merge, keeps doc status lines honest.

The board (design_status.py) is only ever as good as the docs' status lines.
This is the check that catches a doc going stale: when a merge ships code for
a design whose status line wasn't updated in the same merge, or touches a
design doc that has no status line at all, this flags it — and, when a `claude`
CLI is available, asks Haiku to draft the corrected one-line status.

Deterministic detection is free and always runs. Haiku only fires when a
candidate is found (so a clean merge costs nothing), and it only ever *prints
a suggestion* — it never edits or commits, because auto-writing to freshly
merged `main` violates the landing protocol (GIT_TREE_DISCIPLINE.md).

Usage (also called from .git/hooks/post-merge with ORIG_HEAD..HEAD):
    python3 .claude/hooks/design_status_check.py [SINCE] [UNTIL]

Always exits 0 — a housekeeper must never fail a merge.
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
REPO = HOOKS_DIR.parents[1]
DOCS = REPO / "docs"
HAIKU_MODEL = "claude-haiku-4-5-20251001"


def git(*args: str) -> str:
    try:
        out = subprocess.run(["git", *args], cwd=REPO, capture_output=True,
                             text=True, timeout=10)
        return out.stdout.strip()
    except Exception:
        return ""


def status_line(path: Path) -> str | None:
    sys.path.insert(0, str(HOOKS_DIR))
    import design_status
    return design_status.status_line(path)


def find_claude() -> str | None:
    found = shutil.which("claude")
    if found:
        return found
    for cand in (Path.home() / ".local/bin/claude", Path("/usr/local/bin/claude")):
        if cand.exists():
            return str(cand)
    return None


def ask_haiku(claude_bin: str, name: str, current: str | None, subjects: list[str]) -> str:
    """Return a suggested corrected status line, or '' if Haiku says it's fine."""
    prompt = (
        f"A merge just landed these commits that reference design '{name}':\n"
        + "\n".join(f"- {s}" for s in subjects)
        + f"\n\nThe design doc's current status line is:\n"
        + (current or "(no status line at all)")
        + "\n\nIf these commits mean that status line is now stale, incomplete, or "
        "missing, reply with ONLY the corrected one-line status, starting with one "
        "of SHIPPED / IN PROGRESS / APPROVED / PROPOSED. If the status line is "
        "already accurate, reply with exactly: OK"
    )
    try:
        out = subprocess.run([claude_bin, "-p", prompt, "--model", HAIKU_MODEL],
                             cwd=REPO, capture_output=True, text=True, timeout=45)
        reply = out.stdout.strip()
        return "" if reply.upper().startswith("OK") or not reply else reply
    except Exception:
        return ""


def main() -> int:
    since = sys.argv[1] if len(sys.argv) > 1 else (
        "ORIG_HEAD" if git("rev-parse", "--verify", "-q", "ORIG_HEAD") else "HEAD~1")
    until = sys.argv[2] if len(sys.argv) > 2 else "HEAD"
    rng = f"{since}..{until}"

    subjects = [s for s in git("log", rng, "--format=%s").splitlines() if s]
    files = git("diff", "--name-only", rng).splitlines()
    if not subjects:
        return 0

    stems = {p.stem.replace("_DESIGN", ""): p for p in DOCS.glob("*_DESIGN.md")}
    code_touched = any(f.startswith("crates/") for f in files)
    changed_docs = {f for f in files if f.startswith("docs/") and f.endswith("_DESIGN.md")}

    # Which designs did the commit messages name? (they reliably do: "DRAG_CAPTURE P2")
    referenced = {name for name in stems
                  if any(re.search(rf"\b{re.escape(name)}\b", s) for s in subjects)}

    candidates: list[tuple[str, str, str | None, list[str]]] = []
    for name in sorted(referenced):
        path = stems[name]
        doc_rel = f"docs/{path.name}"
        current = status_line(path)
        subs = [s for s in subjects if re.search(rf"\b{re.escape(name)}\b", s)]
        if current is None:
            candidates.append((name, "doc has no status line", current, subs))
        elif code_touched and doc_rel not in changed_docs:
            candidates.append((name, "code shipped, status line not updated in this merge",
                               current, subs))

    if candidates:
        print("\n⚠  design-status housekeeper — possible stale doc status:", file=sys.stderr)
        claude_bin = find_claude() if os.environ.get("DESIGN_STATUS_HAIKU", "1") != "0" else None
        for name, reason, current, subs in candidates:
            print(f"  · {name}: {reason}", file=sys.stderr)
            suggestion = ask_haiku(claude_bin, name, current, subs) if claude_bin else ""
            if suggestion:
                print(f"      Haiku suggests: {suggestion}", file=sys.stderr)
        print("  Backstop only — fix belongs on the branch pre-landing. If it slipped: "
              "update the doc's **Status line IN A WORKTREE and land as a follow-up "
              "merge (main edits are guard-denied).\n", file=sys.stderr)

    bug_backlog_check()
    return 0


def bug_backlog_check() -> None:
    """Same honesty check for the bug tracker: the ``**Status:`` line is the truth, and a
    bug whose named fix-design has SHIPPED (the hole that let BUG-058/059 sit 'open' after
    DRAG_CAPTURE shipped) or that's filed under the wrong section gets a nudge. Print-only,
    whole-file — stays quiet while the backlog is clean. Run: bug_status.py --check."""
    try:
        sys.path.insert(0, str(HOOKS_DIR))
        import bug_status
        problems = bug_status.check(bug_status.BACKLOG.read_text())
    except Exception:
        return
    if not problems:
        return
    print("\n⚠  bug-backlog housekeeper — status drift (bug_status.py):", file=sys.stderr)
    for p in problems:
        print(f"  · {p}", file=sys.stderr)
    print("  Backstop only — reflow belongs on the branch pre-landing. If it slipped: "
          "run the WORKTREE's copy (python3 .claude/worktrees/<name>/.claude/hooks/"
          "bug_status.py --write) and land as a follow-up merge; --write refuses in main.\n",
          file=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
    sys.exit(0)
