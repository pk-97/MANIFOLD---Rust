#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) guard: keep uncommitted work OFF the main
checkout. Agents edit in a git worktree; main stays clean, runnable, and safe
for other sessions (and Peter) to branch from and run.

Why this exists: an agent editing files directly in the main checkout (a) leaves
main un-runnable while breaking changes sit uncommitted — Peter can't build/run
the app — and (b) moves the main working tree and HEAD under any other session or
worktree that branched from it. The fix is structural: never touch the main
checkout's files directly. This hook denies such edits and points the agent at a
worktree.

Denies when the target file resolves INSIDE the main checkout, EXCEPT:
  - files already in a worktree (.claude/worktrees/...) — that's the right place;
  - tooling/meta files under .claude/ (hooks, daemon, commands, settings) — these
    don't affect the app build, and gating them would make editing this very hook
    require a worktree. Repo memory lives outside the project dir and never trips.

The deny repeats on every attempt (no once-per-session sentinel): the only way to
make the edit land is to actually move into a worktree, at which point the target
path is no longer in the main checkout and the guard falls silent on its own.

Fails OPEN on any error or unrecognized shape — never blocks a session on a bug.
A path that resolves cleanly into the main checkout is a deliberate deny, not a
failure.

Receives `{"tool_name", "tool_input": {"file_path": ...}, "cwd": ...}` on stdin.
Emits hookSpecificOutput.permissionDecision="deny" + reason, or nothing.
"""
import json
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


def deny_reason(resolved):
    try:
        rel = resolved.relative_to(_PROJECT_DIR)
    except ValueError:
        rel = resolved
    return (
        f"Blocked: this edit targets `{rel}` in the MAIN checkout. Main is kept "
        f"clean and runnable — agents edit in a git worktree, never directly on "
        f"main (CLAUDE.md, GIT_TREE_DISCIPLINE.md). Create one off the current tip "
        f"and redo the edit there:\n\n"
        f"  git worktree add -b <wave|lane|feat>/<name> .claude/worktrees/<name> HEAD\n\n"
        f"then edit under .claude/worktrees/<name>/ and land back with a --no-ff "
        f"merge. Verify the base is the intended tip first "
        f"(`git -C .claude/worktrees/<name> log --oneline -1`). Tooling files "
        f"under .claude/ are exempt and may be edited in place."
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
    if not in_main_checkout(resolved):
        return 0
    if is_tooling(resolved):
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
