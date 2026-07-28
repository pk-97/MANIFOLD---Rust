#!/usr/bin/env python3
"""PreToolUse(Write) gate: no new landing reports — that history lives in git.

Rule (Peter 2026-07-28, docs-pile class fix): `docs/landings/` is closed to
new files. A landing's story goes in the merge commit message; its status goes
in the design doc header and beads. The 122 existing reports stay as history
and remain Edit-able (on-touch ID naming etc.) — only Write (file creation /
overwrite) is denied there.

Fails open on any error.

Obsolete when: docs/landings/ is deleted outright — with no directory, there is nothing to guard.
"""
import json
import sys


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0
    if data.get("tool_name") != "Write":
        return 0
    path = (data.get("tool_input") or {}).get("file_path") or ""
    if "/docs/landings/" not in path:
        return 0
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": (
                "docs/landings/ is closed to new files (landing-report-guard.py; "
                "Peter 2026-07-28). Landing prose goes in the merge commit message; "
                "status goes in the design doc header + beads. Existing reports are "
                "history — Edit is allowed, Write is not."
            ),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
