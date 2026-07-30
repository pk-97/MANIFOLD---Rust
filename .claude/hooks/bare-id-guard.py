#!/usr/bin/env python3
"""PreToolUse(Edit|Write) gate: IDs carry names — no bare opaque identifiers in prose.

An ID is a join key, not a name. In human-facing markdown, every opaque
identifier carries its human name so a reader (human or agent) never has to
resolve it out of band. Migration is on-touch only: the gate fires on ADDED
text, never on text carried forward, so untouched docs stay valid.

Deterministic contract (this docstring is the spec):

  SCOPE   `.md` files under the repo `docs/` tree, the repo-root `CLAUDE.md`,
          and any `/.claude/projects/*/memory/` directory. Edit: new_string
          only. Write: full content.

  RULE 1  A bead ID (`BUG-` + 2-5 lowercase alphanumerics) must be named at
          least once in the touched text:
              BUG-lu32 (phantom-clip double-commit)
              BUG-297 — multi-session memory exhaustion
          "Named" = the ID followed on its line (closing backtick allowed) by
          `(...)` containing a letter, or an em/en dash then text. One naming
          anywhere in the touched text (Edit: old_string or new_string)
          legitimises every other mention of that ID.

  RULE 2  A cross-doc section ref — `section N` preceded on its line by a
          `.md` filename — must be followed by `(...)` containing a letter:
              docs/WIDGET_TREE_DESIGN.md section 5b (param-surface recipe)
          Same-doc refs stay bare; the heading in the same file names them.

  RULE 3  The `§` symbol is banned in prose outright — write `section N`,
          never the symbol.

  EXEMPT  Fenced code blocks; lines whose first non-space char is `$`; lines
          invoking `bd` (create/show/update/close/list/ready/dep); lines
          containing `external_ref`.

  AUDIT   `bare-id-guard.py --audit` scans the whole corpus read-only and
          prints per-file bare counts (exit 0 always) — for measuring
          migrate-on-touch progress, never for blocking.

Fails open on any error.

Obsolete when: IDs are rendered with their names automatically wherever prose is read
(bd/doc tooling inlines titles), so bare IDs stop costing the reader a lookup.
"""
import json
import re
import sys
from pathlib import Path

ID_RE = re.compile(r"\bBUG-[a-z0-9]{2,5}\b")
SECREF_RE = re.compile(r"\bsection\s+\d[\w.]*")  # digit-led: "section 5b"; word refs self-name
SYMBOL_RE = re.compile(r"§")
FENCE_RE = re.compile(r"^\s*(```|~~~)")
BD_CMD_RE = re.compile(r"\bbd\s+(create|show|update|close|list|ready|dep)\b")
NAME_PAREN_RE = re.compile(r"`?\s*\([^)]*[A-Za-z][^)]*\)")
NAME_DASH_RE = re.compile(r"`?\s*[—–]\s*\S")

REASON = (
    "IDs carry names (CLAUDE.md readability rule; spec: bare-id-guard.py "
    "docstring). This write adds {what} without its human name. Cite as "
    "`BUG-xxxx (short name)` / `BUG-xxxx — short name` (title from `bd show`), "
    "and cross-doc section refs as `FILE.md section N (section name)`. One naming per "
    "touched text is enough; command lines and code blocks are exempt."
)


def prose_lines(text):
    """Yield lines outside fenced code blocks and not otherwise exempt."""
    in_fence = False
    for line in text.splitlines():
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if line.lstrip().startswith("$"):
            continue
        if BD_CMD_RE.search(line) or "external_ref" in line:
            continue
        yield line


def is_named_here(line, start, end):
    return bool(NAME_PAREN_RE.match(line, end) or NAME_DASH_RE.match(line, end))


def named_ids(text):
    out = set()
    for line in prose_lines(text):
        for m in ID_RE.finditer(line):
            if is_named_here(line, m.start(), m.end()):
                out.add(m.group())
    return out


def violations(text, extra_named=frozenset()):
    """Return (bare_ids, bare_secrefs) for the given text."""
    named = named_ids(text) | set(extra_named)
    bare_ids, bare_refs = set(), []
    for line in prose_lines(text):
        for m in ID_RE.finditer(line):
            if m.group() not in named:
                bare_ids.add(m.group())
        for m in SECREF_RE.finditer(line):
            if ".md" in line[: m.start()] and not NAME_PAREN_RE.match(line, m.end()):
                bare_refs.append(m.group())
    return sorted(bare_ids), bare_refs


def in_scope(file_path):
    if not file_path.endswith(".md"):
        return False
    if "/.claude/projects/" in file_path and "/memory/" in file_path:
        return True
    return "/docs/" in file_path or file_path.endswith("/CLAUDE.md")


def audit():
    root = Path(__file__).resolve().parents[2]
    targets = sorted((root / "docs").rglob("*.md")) + [root / "CLAUDE.md"]
    mem = Path.home() / ".claude" / "projects"
    targets += sorted(mem.glob("*/memory/*.md"))
    total_ids = total_refs = 0
    for f in targets:
        try:
            ids, refs = violations(f.read_text(errors="replace"))
        except OSError:
            continue
        if ids or refs:
            total_ids += len(ids)
            total_refs += len(refs)
            print(f"{f}: {len(ids)} unnamed IDs ({', '.join(ids)})"
                  f"{f'; {len(refs)} bare cross-doc section refs' if refs else ''}")
    print(f"total: {total_ids} unnamed IDs, {total_refs} bare cross-doc section refs")
    return 0


def main() -> int:
    if "--audit" in sys.argv:
        return audit()

    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    tool_name = data.get("tool_name")
    if tool_name not in ("Edit", "Write"):
        return 0
    tool_input = data.get("tool_input") or {}
    if not in_scope(tool_input.get("file_path") or ""):
        return 0

    if tool_name == "Edit":
        old = tool_input.get("old_string", "")
        new = tool_input.get("new_string", "")
        # Carried-forward bare IDs stay legal: anything already bare in old
        # text is not "added" by this edit.
        old_ids, old_refs = violations(old)
        ids, refs = violations(new, extra_named=named_ids(old))
        ids = [i for i in ids if i not in old_ids]
        refs = refs[len(old_refs):] if len(refs) > len(old_refs) else []
    else:
        ids, refs = violations(tool_input.get("content", ""))

    what = []
    symbol_hit = tool_name == "Write" and any(
        SYMBOL_RE.search(l) for l in prose_lines(tool_input.get("content", "")))
    if tool_name == "Edit":
        new_l = list(prose_lines(tool_input.get("new_string", "")))
        old_l = list(prose_lines(tool_input.get("old_string", "")))
        symbol_hit = sum("§" in l for l in new_l) > sum("§" in l for l in old_l)
    if symbol_hit:
        what_sym = "the banned § symbol (write `section N` instead)"
    if ids:
        what.append("a bare bead ID (" + ", ".join(ids) + ")")
    if refs:
        what.append("a bare cross-doc section ref (" + ", ".join(refs) + ")")
    if symbol_hit:
        what.append(what_sym)
    if not what:
        return 0

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": REASON.format(what=" and ".join(what)),
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
