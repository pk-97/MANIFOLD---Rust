#!/usr/bin/env python3
"""PostToolUse(Edit|Write|MultiEdit) nudge: catch a newly added, unannotated
`#[allow(dead_code)]` / `#[allow(unused...)]` suppression.

Why this exists: suppression is deferral. CLAUDE.md bans bare markers — every
one must name what un-suppresses it (a design doc, a phase, a wiring task) or
the code should just be deleted. A passive rule in CLAUDE.md is easy to miss
mid-edit; this hook fires the moment a bare marker lands.

Fires ONLY when the edit *adds* a marker that wasn't already present:
  - Edit: pattern matches somewhere in new_string but nowhere in old_string.
  - MultiEdit: same check, over the old_string/new_string of each edit in
    `edits`, joined together (so a marker moved from one edit's old_string to
    another edit's new_string within the same call still reads as pre-existing).
  - Write: pattern matches somewhere in content. Write has no "before" text,
    so a full-file rewrite that merely carries forward an existing marker is
    a false positive — accepted, since this hook is advisory only.

A matched marker is exempt if its own attribute line, or any of the up-to-2
lines directly above it, contains a `//` comment — the heuristic for "names
its un-suppression trigger". Only Rust files (`.rs`) are considered.

Never blocks: only ever emits `additionalContext`, never a permissionDecision
(PostToolUse can't undo the write anyway). Fails open on any error.

Receives `{"tool_name": "Edit"|"Write"|"MultiEdit", "tool_input": {...}}` on
stdin. Emits hookSpecificOutput.additionalContext, or nothing.
"""
import json
import re
import sys

MARKER_RE = re.compile(r"#\[allow\((dead_code|unused[A-Za-z_]*)\b")

NUDGE = (
    "You just added #[allow(dead_code)] with no reason. Suppression is deferral: "
    "either delete the code now, or annotate the attribute with what un-suppresses "
    "it — a design doc, a phase, a wiring task. Bare suppressions are banned in "
    "this repo (CLAUDE.md)."
)


def has_marker(text: str) -> bool:
    return bool(text) and bool(MARKER_RE.search(text))


def unannotated_marker_present(text: str) -> bool:
    """True if `text` contains a marker whose attribute line (and the up-to-2
    lines above it) carry no `//` comment."""
    if not text:
        return False
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if not MARKER_RE.search(line):
            continue
        window = lines[max(0, i - 2) : i + 1]
        if not any("//" in w for w in window):
            return True
    return False


def extract_old_new(tool_name: str, tool_input: dict):
    """Return (old_text, new_text) — the concatenated before/after snippets
    this call touched — or (None, None) if the shape is unrecognized."""
    if tool_name == "Edit":
        return tool_input.get("old_string", ""), tool_input.get("new_string", "")
    if tool_name == "MultiEdit":
        edits = tool_input.get("edits") or []
        olds = "\n".join(e.get("old_string", "") for e in edits)
        news = "\n".join(e.get("new_string", "") for e in edits)
        return olds, news
    if tool_name == "Write":
        return None, tool_input.get("content", "")
    return None, None


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
    if not file_path.endswith(".rs"):
        return 0

    old_text, new_text = extract_old_new(tool_name, tool_input)
    if new_text is None:
        return 0

    if old_text is not None:
        # Edit / MultiEdit: only an ADDED marker counts.
        if not has_marker(new_text) or has_marker(old_text):
            return 0
    else:
        # Write: no "before" to diff against.
        if not has_marker(new_text):
            return 0

    if not unannotated_marker_present(new_text):
        return 0

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": NUDGE,
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
