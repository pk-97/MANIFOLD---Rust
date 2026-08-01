#!/usr/bin/env python3
"""PreToolUse(Bash) gate: a merge into main declares which beads it closes.

Rule: a fix that ships without touching the tracker leaves a closed bug
sitting open. Landing is the one moment the lead knows what actually shipped,
so the merge message must carry a trailer:

    Closes: BUG-abcd, BUG-efgh
    Closes: none

`none` is always available on purpose. The point is that the question gets
answered once per landing and the answer lands in git where it can be audited,
not that the machine can tell a real fix from a refactor — it cannot.

Scope: `git merge` whose target is main, run from the main checkout. Merges into
a lane or wave branch (integrating origin/main) are exempt: they ship nothing.

Fails open on any error.

Obsolete when: beads are closed by the same command that lands, so the trailer
and the close are one act and there is nothing to forget.
"""
import json
import re
import shlex
import subprocess
import sys

TRAILER = re.compile(r"^\s*Closes:\s*(.+?)\s*$", re.MULTILINE | re.IGNORECASE)
BEAD = re.compile(r"^(BUG|TASK)-[0-9a-z]+$", re.IGNORECASE)


def current_branch() -> str:
    try:
        r = subprocess.run(["git", "rev-parse", "--abbrev-ref", "HEAD"],
                           capture_output=True, text=True, timeout=10)
        return r.stdout.strip()
    except Exception:
        return ""


def deny(reason: str) -> int:
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
    return 0


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0
    if data.get("tool_name") != "Bash":
        return 0
    command = (data.get("tool_input") or {}).get("command") or ""
    if "git merge" not in command:
        return 0
    # Only a merge landing ON main. Merging origin/main INTO a branch ships
    # nothing and must stay frictionless — it is half the landing protocol.
    if current_branch() != "main":
        return 0
    try:
        tokens = shlex.split(command)
    except ValueError:
        return 0
    if "merge" not in tokens:
        return 0
    merged = [t for t in tokens[tokens.index("merge") + 1:] if not t.startswith("-")]
    if any(t in ("origin/main", "main") for t in merged):
        return 0

    # Search the SHLEX-PARSED tokens, never the raw command: the closing shell
    # quote rides along on the raw text and turns `none` into `none"`.
    hit = next(filter(None, (TRAILER.search(t) for t in tokens)), None)
    if not hit:
        return deny(
            "This merge lands on main and its message has no `Closes:` trailer "
            "(closes-trailer-guard.py). Add one line naming the beads this landing "
            "closes, or `Closes: none` if it closes nothing:\n"
            "    Closes: BUG-abcd (short name), BUG-efgh (short name)\n"
            "    Closes: none\n"
            "Then run `bd close <id>` for each one after the push. A fix that "
            "ships without the tracker being told stays open forever and comes "
            "back as somebody's next task."
        )
    value = hit.group(1)
    if value.strip().lower() == "none":
        return 0
    ids = [p.strip() for p in re.split(r"[,\s]+", re.sub(r"\([^)]*\)", "", value)) if p.strip()]
    bad = [i for i in ids if not BEAD.match(i)]
    if not ids or bad:
        return deny(
            f"The `Closes:` trailer does not parse as bead ids: {value!r} "
            "(closes-trailer-guard.py). Use `Closes: BUG-abcd (short name)` — ids "
            "comma-separated, human names in parens — or `Closes: none`."
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
