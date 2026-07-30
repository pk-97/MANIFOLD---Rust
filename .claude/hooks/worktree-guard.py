#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit): keep uncommitted work OFF the main checkout.

Main stays clean and runnable for Peter and for any session branching from it.
Agents edit in a worktree. Denies when the target resolves inside the main
checkout, except:
  - paths already under .claude/worktrees/;
  - tooling under .claude/ (hooks, commands, settings) — no effect on the app
    build, and gating them would make editing this hook require a worktree.
    Repo memory lives outside the project dir and never trips;
  - conflicted files while .git/MERGE_HEAD exists — landing merges happen in the
    main checkout. Scope: merge_conflict_paths();
  - docs/**/*.md, all of them including *_DESIGN.md. Adding or renaming a doc
    still needs gen_docs_index.py in the same commit.

The deny repeats on every attempt: moving into a worktree makes the path stop
matching, so the guard falls silent on its own.

Fails OPEN on any error or unrecognized shape. A path that resolves cleanly into
the main checkout is a deliberate deny.

In: {"tool_name", "tool_input": {"file_path"}, "cwd"}. Out:
hookSpecificOutput.permissionDecision="deny" + reason, or nothing.

Obsolete when: the main checkout stops being the shared runnable trunk.
"""
import json
import subprocess
import sys
from pathlib import Path

# __file__ is <main>/.claude/hooks/worktree-guard.py; parents[2] is the main
# checkout root. settings.json invokes the hook via $CLAUDE_PROJECT_DIR, so even
# a session working inside a worktree runs THIS (main) copy — _PROJECT_DIR is
# always the true main root. Same derivation preToolUseBash.py relies on.
_PROJECT_DIR = Path(__file__).resolve().parents[2]
_WORKTREES_DIR = _PROJECT_DIR / ".claude" / "worktrees"
_CLAUDE_DIR = _PROJECT_DIR / ".claude"


def resolve_target(file_path, cwd):
    """Absolute, resolved target path, or None if unusable. A relative path is
    joined to cwd (the session's working dir), falling back to the main root."""
    if not file_path:
        return None
    p = Path(file_path)
    if not p.is_absolute():
        p = (Path(cwd) if cwd else _PROJECT_DIR) / p
    try:
        return p.resolve()  # strict=False: works for not-yet-created files
    except OSError:
        return None


def in_main_checkout(resolved):
    in_main = resolved == _PROJECT_DIR or _PROJECT_DIR in resolved.parents
    in_worktrees = resolved == _WORKTREES_DIR or _WORKTREES_DIR in resolved.parents
    return in_main and not in_worktrees


def is_tooling(resolved):
    return resolved == _CLAUDE_DIR or _CLAUDE_DIR in resolved.parents


_DOCS_DIR = _PROJECT_DIR / "docs"


def is_doc_fast_path(resolved):
    """docs/**/*.md — all docs, including *_DESIGN.md (widened by Peter
    2026-07-24: the exclusion was too conservative; doc edits don't break the
    build, and status lines must stay cheap to keep true). Root CLAUDE.md is
    also fast-path (Peter 2026-07-25: "you don't need a worktree for doc
    updates anymore")."""
    if resolved.suffix != ".md":
        return False
    if resolved == _PROJECT_DIR / "CLAUDE.md":
        return True
    return resolved == _DOCS_DIR or _DOCS_DIR in resolved.parents


def merge_conflict_paths():
    """Resolved paths with unmerged index entries during an in-progress merge in
    the MAIN checkout. Empty set when no merge is live or on ANY error — the
    carve-out only opens on positive evidence; an error here restores the plain
    deny, never widens the exemption. Cheap on the common path: one stat of
    .git/MERGE_HEAD; the subprocess runs only mid-merge."""
    if not (_PROJECT_DIR / ".git" / "MERGE_HEAD").exists():
        return set()
    try:
        out = subprocess.run(
            ["git", "-C", str(_PROJECT_DIR), "diff", "--name-only", "--diff-filter=U"],
            capture_output=True, text=True, timeout=5,
        )
        if out.returncode != 0:
            return set()
        return {
            (_PROJECT_DIR / line).resolve()
            for line in out.stdout.splitlines()
            if line.strip()
        }
    except Exception:
        return set()


def is_verdicts_path(resolved):
    """True if resolved path is under ANY `.claude/orchestration/verdicts/`
    directory, covering both the main checkout and worktree locations (I2).

    gate_runner is the only writer of verdict files; direct Edit/Write/MultiEdit
    to the trail is always the wrong path — even for gate_runner itself, which
    appends via Python `open()`.
    """
    parts = resolved.parts
    for i, part in enumerate(parts):
        if part == ".claude" and i + 2 < len(parts):
            if parts[i + 1] == "orchestration" and parts[i + 2] == "verdicts":
                return True
    return False


def deny_reason(resolved):
    try:
        rel = resolved.relative_to(_PROJECT_DIR)
    except ValueError:
        rel = resolved
    return (
        f"Blocked: this edit targets `{rel}` in the MAIN checkout. Main is kept "
        f"clean and runnable — agents edit in a git worktree, never directly on "
        f"main (CLAUDE.md, GIT_TREE_DISCIPLINE.md). Acquire a slot from the ring "
        f"and redo the edit there:\n\n"
        f"  scripts/agent-worktree.py acquire <task-label> "
        f"<wave|lane|feat>/<name> --tip HEAD\n\n"
        f"then edit under the printed slot path and land back with a --no-ff "
        f"merge. Verify the base is the intended tip first (the acquire output's "
        f"HEAD line). Raw `git worktree add` is denied by hook (455 GB incident, "
        f"2026-07-15). Tooling files "
        f"under .claude/ are exempt and may be edited in place. During an "
        f"in-progress merge in main, only files git lists as unmerged are "
        f"editable (conflict resolution per GIT_TREE_DISCIPLINE §2)."
    )


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    if data.get("tool_name") not in ("Edit", "Write", "MultiEdit"):
        return 0

    tool_input = data.get("tool_input") or {}
    resolved = resolve_target(tool_input.get("file_path") or "", data.get("cwd") or "")
    if resolved is None:
        return 0

    # I2: verdicts are written only by gate_runner, never via Edit|Write|MultiEdit.
    if is_verdicts_path(resolved):
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": (
                    "Blocked: verdicts are written only by gate_runner — direct "
                    "Edit/Write/MultiEdit to the verdicts trail is never correct. "
                    "gate_runner appends via Python `open()`, not through the Edit "
                    "tool. Path: " + str(resolved)
                ),
            }
        }))
        return 0

    if not in_main_checkout(resolved):
        return 0
    if is_tooling(resolved):
        return 0
    if is_doc_fast_path(resolved):
        return 0
    if resolved in merge_conflict_paths():
        return 0

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": deny_reason(resolved),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
