#!/usr/bin/env python3
"""
Conversation Recall Hook — UserPromptSubmit

Injects recent conversation digests on the first message of each session
so Claude always has context on prior work.

Incrementally indexes any new .jsonl conversation files, then returns
the most recent sessions.
"""

import json
import subprocess
import sys
from pathlib import Path

# Co-located scripts in the same hooks directory
HOOKS_DIR = Path(__file__).resolve().parent
PROJECT_DIR = Path.home() / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust"
DIGEST_DIR = PROJECT_DIR / "digests"
LOCK_DIR = Path.home() / ".claude/cache/recall-locks"


def already_fired(session_id):
    """Check if recall already fired for this session."""
    if not session_id:
        return False
    LOCK_DIR.mkdir(parents=True, exist_ok=True)
    lock_file = LOCK_DIR / f"{session_id}.lock"
    if lock_file.exists():
        return True
    lock_file.touch()
    return False


def cleanup_old_locks():
    """Remove lock files older than 24 hours to prevent buildup."""
    if not LOCK_DIR.exists():
        return
    import time
    cutoff = time.time() - 86400
    for f in LOCK_DIR.glob("*.lock"):
        try:
            if f.stat().st_mtime < cutoff:
                f.unlink()
        except OSError:
            pass


def index_new_conversations():
    """Run the indexer on any new conversations (incremental, fast)."""
    try:
        subprocess.run(
            [sys.executable, str(HOOKS_DIR / "index-conversations.py")],
            capture_output=True,
            timeout=10,
        )
    except Exception:
        pass  # Don't block on indexing failures


def get_status_board():
    """Live design-status board, generated from docs/*_DESIGN.md (never memory).

    The docs' status lines are the single source of truth; this injects a fresh
    board each session so status can't be read from a stale memory copy. Fail
    open — a board error must never block the (more important) recall context.
    """
    try:
        sys.path.insert(0, str(HOOKS_DIR))
        import design_status
        return design_status.build_board()
    except Exception:
        return ""


def get_recent_sessions(prompt=""):
    """Get recent conversation digests, with optional prompt for continuation detection."""
    try:
        cmd = [sys.executable, str(HOOKS_DIR / "search-conversations.py")]
        if prompt:
            cmd.append(prompt)
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=5,
        )
        return result.stdout.strip()
    except Exception:
        return ""


def main():
    # Read hook input from stdin
    try:
        hook_input = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        sys.exit(0)

    prompt = hook_input.get("prompt", "")
    session_id = hook_input.get("session_id", "") or hook_input.get("sessionId", "")

    # Only fire once per session — but don't create lock if prompt is empty
    if already_fired(session_id):
        sys.exit(0)

    if not prompt.strip():
        if session_id:
            lock_file = LOCK_DIR / f"{session_id}.lock"
            lock_file.unlink(missing_ok=True)
        sys.exit(0)

    # Housekeeping
    cleanup_old_locks()

    # Incremental index (only processes new .jsonl files)
    index_new_conversations()

    # Always return recent sessions, pass prompt for continuation detection
    context = get_recent_sessions(prompt)

    # Prepend the live design-status board so status is read from the docs, not
    # from stale memory. Board first (it's the ground truth), recall after.
    board = get_status_board()
    if board:
        context = f"{board}\n\n{context}" if context else board

    if context:
        output = {
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": context,
            }
        }
        print(json.dumps(output))

    sys.exit(0)


if __name__ == "__main__":
    main()
