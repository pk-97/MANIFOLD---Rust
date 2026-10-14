#!/usr/bin/env python3
"""PreToolUse(Edit|Write) gate: rules are written as plain rules, without history.

Two checks, both on ADDED text only (Edit: new_string minus what old_string
already carried), both deny. Fails open on any error.

1. History in memory. Files under a `.claude/projects/*/memory/` directory hold
   current rules and non-derivable context only; status lives in beads and the
   design status board, history in git. The corpus regrows through two
   machine-detectable vectors:
     - commit hashes  — git-derivable history; `git log -S` finds them
     - status markers — LANDED/SHIPPED/CLOSED/MERGED/DONE/COMPLETE stamps

2. Attribution and dated provenance in rule text. CLAUDE.md, any `docs/**.md`
   and any `.claude/**.md` (memory, agents, context nudges, discipline docs)
   state the rule, not who said it or when: "Peter said", "Peter ruled",
   "per Peter", "(Peter, 2026-10-13)", "Peter approved 2026-07-31". A rule's why
   is its reason, not its author. The one dated provenance line CLAUDE.md
   section Voice allows (where the why cannot be derived) names the event, not
   the person, so it passes.

Exempt where who-decided is the point: `guide_decision_log.md` (both checks),
and numbered D-entry lines in `docs/*_DESIGN.md` (check 2).

Obsolete when: memory and rule docs are generated from source-of-truth stores
instead of hand-written, so neither status nor attribution can drift into them.
"""
import json
import re
import sys

# at least one digit so plain words in hex alphabet ("deadbeef" still hits,
# "accede" doesn't)
HASH_RE = re.compile(r"\b(?=[0-9a-f]*[0-9])[0-9a-f]{7,40}\b")
STATUS_RE = re.compile(r"\b(LANDED|SHIPPED|CLOSED|MERGED|COMPLETE|DONE)\b")

_DATE = r"\d{4}-\d{2}-\d{2}"
ATTRIBUTION_RES = (
    # Past tense only: "if Peter says go" and "a branch Peter approved" are rules.
    re.compile(r"\bPeter (?:said|ruled|decided|re-approved|confirmed)\b"),
    re.compile(rf"\bPeter approved,? \(?{_DATE}"),
    re.compile(r"\b[Pp]er Peter\b"),
    re.compile(r"\b[Dd]ecided with Peter\b"),
    re.compile(rf"\bPeter[,:]?\s*\(?{_DATE}"),
)

# A numbered design decision: "D-12", "**D12**", "- D7:", "### D-3a", "| D-4 |".
D_ENTRY_RE = re.compile(r"^\s*(?:[-*#>|]+\s*)*(?:\*\*)?D-?\d+[a-z]?\b")

EXEMPT_BASENAMES = {"guide_decision_log.md"}

HISTORY_REASON = (
    "Memory is for current rules, not history (CLAUDE.md). This write adds "
    "{what} to a memory file. Commit hashes and landed/shipped/closed status "
    "belong in git, beads (`bd`), or the design doc status header — never in "
    "memory. Rewrite the memory as the current rule/fact without the "
    "provenance, or put the status where it lives."
)

ATTRIBUTION_REASON = (
    "Rules are written as plain rules (CLAUDE.md section Voice; spec: "
    "memory-history-guard.py). This write adds attribution or dated provenance: "
    "{what}. State the rule and its reason; drop who said it and when. "
    "Exempt: guide_decision_log.md and numbered D-entries in docs/*_DESIGN.md."
)


def is_memory(path: str) -> bool:
    return "/.claude/projects/" in path and "/memory/" in path


def is_rule_doc(path: str) -> bool:
    if not path.endswith(".md"):
        return False
    return (path.rsplit("/", 1)[-1] == "CLAUDE.md" or "/docs/" in path
            or "/.claude/" in path)


def history_hits(text: str):
    hits = []
    if HASH_RE.search(text):
        hits.append("a commit hash")
    if STATUS_RE.search(text):
        hits.append("a status marker (LANDED/SHIPPED/CLOSED/MERGED/DONE/COMPLETE)")
    return hits


def attribution_hits(text: str, path: str):
    """Every attribution phrase in `text`, D-entry lines of design docs skipped."""
    if path.endswith("_DESIGN.md") and "/docs/" in path:
        text = "\n".join(line for line in text.splitlines() if not D_ENTRY_RE.match(line))
    return [m.group(0) for r in ATTRIBUTION_RES for m in r.finditer(text)]


def added(old_hits, new_hits):
    """Hits in the new text beyond what the old text already carried."""
    remaining = list(old_hits)
    out = []
    for hit in new_hits:
        if hit in remaining:
            remaining.remove(hit)
        else:
            out.append(hit)
    return out


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

    tool_name = data.get("tool_name")
    if tool_name not in ("Edit", "Write"):
        return 0

    tool_input = data.get("tool_input") or {}
    path = tool_input.get("file_path") or ""
    if path.rsplit("/", 1)[-1] in EXEMPT_BASENAMES:
        return 0
    if tool_name == "Edit":
        old, new = tool_input.get("old_string", ""), tool_input.get("new_string", "")
    else:
        old, new = "", tool_input.get("content", "")

    if is_memory(path):
        hits = [h for h in history_hits(new) if h not in history_hits(old)]
        if hits:
            return deny(HISTORY_REASON.format(what=" and ".join(hits)))

    if is_rule_doc(path):
        hits = added(attribution_hits(old, path), attribution_hits(new, path))
        if hits:
            return deny(ATTRIBUTION_REASON.format(what=", ".join(f'"{h}"' for h in hits)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
