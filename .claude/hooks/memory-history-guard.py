#!/usr/bin/env python3
"""PreToolUse(Edit|Write) gate: block history-writes into the memory directory.

Rule (CLAUDE.md / 2026-07-27 purge): memory holds current rules and
non-derivable context only. Status lives in beads + the DESIGN STATUS BOARD;
history lives in git. The 338->224 purge showed the corpus regrows through two
vectors, both machine-detectable:

  1. commit hashes  — git-derivable history; `git log -S` finds them
  2. status markers — LANDED/SHIPPED/CLOSED/MERGED/DONE/COMPLETE as
                      uppercase status stamps

Fires only on ADDED text (Edit: in new_string, not old_string), only for
files under a `.claude/projects/*/memory/` directory. `guide_decision_log.md`
is exempt — dated, settled decisions are its purpose. Fails open on any error.

Obsolete when: memory files are generated from source-of-truth stores instead of hand-written, so status can no longer drift into them.
"""
import json
import re
import sys

# at least one digit so plain words in hex alphabet ("deadbeef" still hits,
# "accede" doesn't)
HASH_RE = re.compile(r"\b(?=[0-9a-f]*[0-9])[0-9a-f]{7,40}\b")
STATUS_RE = re.compile(r"\b(LANDED|SHIPPED|CLOSED|MERGED|COMPLETE|DONE)\b")

EXEMPT_BASENAMES = {"guide_decision_log.md"}

REASON = (
    "Memory is for current rules, not history (CLAUDE.md). This write adds "
    "{what} to a memory file. Commit hashes and landed/shipped/closed status "
    "belong in git, beads (`bd`), or the design doc status header — never in "
    "memory. Rewrite the memory as the current rule/fact without the "
    "provenance, or put the status where it lives."
)


def offending(text: str):
    hits = []
    if HASH_RE.search(text):
        hits.append("a commit hash")
    if STATUS_RE.search(text):
        hits.append("a status marker (LANDED/SHIPPED/CLOSED/MERGED/DONE/COMPLETE)")
    return hits


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    tool_name = data.get("tool_name")
    if tool_name not in ("Edit", "Write"):
        return 0

    tool_input = data.get("tool_input") or {}
    file_path = tool_input.get("file_path") or ""
    if "/.claude/projects/" not in file_path or "/memory/" not in file_path:
        return 0
    if file_path.rsplit("/", 1)[-1] in EXEMPT_BASENAMES:
        return 0

    if tool_name == "Edit":
        old = tool_input.get("old_string", "")
        new = tool_input.get("new_string", "")
        hits = [h for h in offending(new) if h not in offending(old)]
    else:
        hits = offending(tool_input.get("content", ""))

    if not hits:
        return 0

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": REASON.format(what=" and ".join(hits)),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
