#!/usr/bin/env python3
"""PreToolUse hook for AskUserQuestion: the daemon's deterministic
pre-question tier (DESIGN.md §2c-ask).

Why this exists, and why it can't be the async observer: on 2026-07-04 an
orchestrator hit a "cheap approximation vs. proper primitive" fork and asked
the user with the shortcut marked "(Recommended)" — the shortcut-as-
recommendation framing CLAUDE.md's fix-at-the-root rule forbids. The daemon
never caught it: a question-wait produces no tool events, so nothing revived
the observer during the 12 minutes it sat idle-exited, and even a live
observer's whisper lands after the question is already blocking the session.
This hook runs synchronously, before the question ever renders.

Behavior: `common.detect_shortcut_fork` screens the question's own option
text (no classifier call — deterministic regex, same tier as
`detect_stopgap_markers`). On a match, deny once per distinct question (a
hash-keyed marker file lets an identical re-ask through — never bounce the
same question twice) with a reason quoting fix-at-the-root. Also revives the
observer via `valve.ensure_observer`, since a question-wait is otherwise a
blind spot for the idle-exit revival path.

Fails open on any error: this hook must never be able to block a session.
"""
import hashlib
import json
import os
import sys

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

BOUNCE_DIR = os.path.join(DAEMON_DIR, "verdicts", "ask_question_bounced")


def _question_hash(questions):
    raw = json.dumps(questions, sort_keys=True, default=str)
    return hashlib.sha256(raw.encode("utf-8", "replace")).hexdigest()


def main():
    try:
        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        tool_input = data.get("tool_input") or {}

        try:
            import valve

            valve.ensure_observer(session_id, data.get("transcript_path"))
        except Exception:
            pass

        import common

        hits = common.detect_shortcut_fork(tool_input)
        if not hits:
            return

        questions = tool_input.get("questions")
        key = _question_hash(questions)
        os.makedirs(BOUNCE_DIR, exist_ok=True)
        marker_path = os.path.join(BOUNCE_DIR, f"{key}.bounced")

        if os.path.exists(marker_path):
            # Already bounced once — this is the re-ask. Let it through.
            return

        with open(marker_path, "w", encoding="utf-8") as f:
            f.write(session_id or "")

        reason = (
            "This question offers a cheaper option marked (Recommended) alongside "
            "a proper/root-fix option — the shortcut-as-recommendation framing "
            "CLAUDE.md's fix-at-the-root rule forbids. The root fix is the default "
            "recommendation; if it genuinely can't ship this session, say so "
            "explicitly rather than recommending the stopgap. Proceed with the "
            "root fix, or re-ask only if this is a genuine scope, taste, or "
            "destructive-action call — not a cost tradeoff."
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                }
            )
        )
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
