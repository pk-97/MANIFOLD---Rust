#!/usr/bin/env python3
"""Bug-backlog status tool — makes the ``**Status:`` line the single source of truth.

``docs/BUG_BACKLOG.md`` used to encode a bug's status in three places that drift:
the ``## Open`` / ``## Fixed`` section it sits under, free text in the heading, and
the index table at the top. This tool moves the truth into one place — a
``**Status:`` line under each ``### BUG-NNN`` heading — mirroring how design docs
carry a ``**Status:`` line that ``design_status.py`` parses.

Two modes:

    python3 .claude/hooks/bug_status.py            # --check: report drift, exit 1 if any
    python3 .claude/hooks/bug_status.py --write     # insert missing Status lines + reflow
                                                     # entries into Open/Fixed by status,
                                                     # behind a content-fidelity guard

``--check`` is what the post-merge housekeeper (design_status_check.py) calls: it
never edits, it prints nudges. Following the same rule as the design-status
housekeeper, nothing here auto-writes to a freshly merged tree — ``--write`` is a
deliberate, human-run reflow.

The index table is intentionally NOT regenerated: its one-liners are hand-curated
(richer than the headings) and it carries index-only design items (e.g. BUG-080)
that have no entry. ``--check`` reports index/entry drift instead of flattening it.

**Closed-bug archive (added 2026-07-12):** ``## Fixed`` no longer holds every closed
bug as a full ``### BUG-NNN`` entry forever — periodically (not per-fix) the full
write-ups are swept verbatim into ``docs/archive/BUG_BACKLOG_CLOSED.md`` and ``##
Fixed`` keeps a one-line ``- BUG-NNN (slug) — STATUS ... — full history in
docs/archive/...`` pointer per archived bug instead. The per-fix workflow is
unchanged (set ``**Status:`` to FIXED, run ``--write`` to reflow the *full* entry
into ``## Fixed`` as before) — a bug lives there as a complete write-up until the
next archive sweep turns it into a pointer. ``--check`` treats both shapes as valid
under ``## Fixed`` and cross-checks the pointers against the archive file: every
pointer must resolve to a real archived entry, every archived entry must have a
live pointer, and no id may exist as a full entry in both places at once.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
REPO = HOOKS_DIR.parents[1]
BACKLOG = REPO / "docs" / "BUG_BACKLOG.md"
ARCHIVE = REPO / "docs" / "archive" / "BUG_BACKLOG_CLOSED.md"

POINTER_RE = re.compile(r"^-\s+(BUG-[0-9A-Za-z-]+)\s+\(")

# Statuses that mean "resolved" — these belong under ## Fixed and out of the open index.
RESOLVED = {"FIXED", "SUPERSEDED"}
# Everything unresolved stays under ## Open and in the index (incl. parked/deferred watch items).
ACTIVE = {"OPEN", "PARTIAL", "REOPENED", "PARKED", "DEFERRED"}
ALL_STATUSES = RESOLVED | ACTIVE

HEADING_RE = re.compile(r"^### (BUG-[0-9A-Za-z-]+)\b")
STATUS_RE = re.compile(r"^\*\*Status:\*\*\s*([A-Z]+)")
DESIGN_RE = re.compile(r"\b([A-Z0-9][A-Z0-9_]*_DESIGN)(?:\.md)?\b")
# A design named as the fix: a fix keyword within ~80 chars before the design token.
FIX_DESIGN_RE = re.compile(
    r"(?is)(?:root fix|fix shape|fix is|fixed by|fix:)[^.]{0,80}?\b([A-Z0-9][A-Z0-9_]*_DESIGN)\b")
# Explicit waiver for the shipped-design nudge: an entry line
# `Verified-open-despite: SOME_DESIGN (reason)` says a human already did the
# "verify + mark" step and the bug is genuinely still open even though that
# design shipped (e.g. BUG-118: shafts shipped, Peter froze the bug before the
# confirming re-render). The reason parenthetical is prose, not parsed.
WAIVER_RE = re.compile(r"(?im)^Verified-open-despite:\s*([A-Z0-9][A-Z0-9_]*_DESIGN)\b")


def derive_status(heading: str) -> str:
    """Classify a bug from its heading text. The order matters — most specific first."""
    up = heading.upper()
    if "REOPENED" in up:
        return "REOPENED"
    if "PARTIALLY FIXED" in up or re.search(r"\bPARTIAL\b", up):
        return "PARTIAL"
    if "SUPERSEDED" in up:
        return "SUPERSEDED"
    if re.search(r"\bFIXED\b", up):
        return "FIXED"
    if "PARKED" in up:
        return "PARKED"
    if "DEFERRED" in up or "DESIGN GAP" in up:
        return "DEFERRED"
    return "OPEN"


def fix_ref(heading: str) -> str:
    """Pull a `@ <sha>` or trailing date out of a FIXED heading, for the Status line."""
    m = re.search(r"@\s*([0-9a-f]{7,40})", heading)
    if m:
        return f" @ {m.group(1)}"
    m = re.search(r"FIXED[^0-9]*(\d{4}-\d{2}-\d{2})", heading, re.I)
    if m:
        return f" ({m.group(1)})"
    return ""


class Entry:
    def __init__(self, bug_id: str, lines: list[str], section: str):
        self.id = bug_id
        self.lines = lines            # includes the ### heading, excludes trailing blank sep
        self.section = section        # "Open" or "Fixed" — where it physically lives now

    @property
    def heading(self) -> str:
        return self.lines[0]

    @property
    def declared_status(self) -> str | None:
        for l in self.lines[1:4]:
            m = STATUS_RE.match(l)
            if m:
                return m.group(1)
        return None

    @property
    def status(self) -> str:
        return self.declared_status or derive_status(self.heading)

    @property
    def fix_designs(self) -> set[str]:
        """Designs named as this bug's FIX (root fix / fix shape lines), not merely mentioned.

        The distinction matters: BUG-056/057 name a design in a "found while gating …"
        line; BUG-058/059 name it as "Root fix is …". Only the latter should nudge.
        Matched across line wraps: a fix keyword within ~80 chars before the design name.
        """
        text = re.sub(r"\s+", " ", "\n".join(self.lines))
        out: set[str] = set()
        for m in FIX_DESIGN_RE.finditer(text):
            out.add(m.group(1))
        return out

    def body_signature(self) -> tuple:
        """Content identity for the fidelity guard: every non-blank line that isn't the
        **Status: line. Blank lines are formatting, not content — ignoring them lets the
        inserted Status line + its separator not register as a content change, while any
        real dropped/altered text still trips the guard."""
        return tuple(l for l in self.lines if l.strip() and not STATUS_RE.match(l))

    def with_status_line(self) -> list[str]:
        """Return lines with a **Status: line guaranteed right after the heading."""
        if self.declared_status:
            return list(self.lines)
        st = derive_status(self.heading)
        ref = fix_ref(self.heading) if st == "FIXED" else ""
        rest = self.lines[1:]
        sep = [] if (rest and rest[0].strip() == "") else [""]  # avoid a double blank
        return [self.lines[0], f"**Status:** {st}{ref}"] + sep + rest


def parse(text: str):
    """Split into (head, entries, tail, strays, pointers).

    Entries span the Open+Fixed region in file order. Archive-pointer lines
    (POINTER_RE) are their own bucket wherever they appear — never entry body,
    never strays — so rebuild() can re-emit them (BUG-139: they used to be
    swallowed into a preceding entry's body or dropped as strays)."""
    lines = text.split("\n")
    idx = {i: l.strip() for i, l in enumerate(lines) if l.startswith("## ")}
    open_i = next(i for i, l in idx.items() if l == "## Open")
    fixed_i = next(i for i, l in idx.items() if l == "## Fixed")
    tail_i = min(i for i in idx if i > fixed_i)

    head = lines[:open_i]                 # everything up to and including the index
    tail = lines[tail_i:]                 # ## Checked and safe … onward

    entries: list[Entry] = []
    strays: list[str] = []                # non-entry, non-blank lines (e.g. "Next free id:")
    pointers: list[str] = []              # one-line closed-bug archive pointers
    i = open_i + 1
    cur_section = "Open"
    while i < tail_i:
        line = lines[i]
        if i == fixed_i:
            cur_section = "Fixed"
            i += 1
            continue
        m = HEADING_RE.match(line)
        if m:
            j = i + 1
            block = [line]
            while (j < tail_i and j != fixed_i and not HEADING_RE.match(lines[j])
                   and not POINTER_RE.match(lines[j].strip())):
                block.append(lines[j])
                j += 1
            while block and block[-1].strip() == "":   # trim trailing blanks
                block.pop()
            entries.append(Entry(m.group(1), block, cur_section))
            i = j
        else:
            if POINTER_RE.match(line.strip()):
                pointers.append(line)
            elif line.strip() and not line.startswith("## "):
                strays.append(line)
            i += 1
    return head, entries, tail, strays, pointers


def rebuild(head, entries, tail, pointers) -> str:
    active = [e for e in entries if e.status not in RESOLVED]
    resolved = [e for e in entries if e.status in RESOLVED]
    out = list(head)
    out += ["## Open", ""]
    for e in active:
        out += e.with_status_line() + [""]
    out += ["## Fixed", ""]
    for e in resolved:
        out += e.with_status_line() + [""]
    if pointers:
        out += list(pointers) + [""]
    out += tail
    return "\n".join(out)


def index_ids(head_lines) -> list[str]:
    ids = []
    for l in head_lines:
        m = re.match(r"^\|\s*(BUG-[0-9A-Za-z /]+?)\s*\|", l)
        if m:
            for part in re.split(r"\s*/\s*", m.group(1).strip()):
                part = part.strip()
                if part.startswith("BUG-"):
                    ids.append(part)
                elif re.fullmatch(r"\d+", part):
                    ids.append("BUG-" + part)
    return ids


def archive_ids() -> set[str]:
    """IDs of ``### BUG-NNN`` entries archived to docs/archive/BUG_BACKLOG_CLOSED.md."""
    if not ARCHIVE.exists():
        return set()
    ids = set()
    for l in ARCHIVE.read_text().split("\n"):
        m = HEADING_RE.match(l)
        if m:
            ids.add(m.group(1))
    return ids


def pointer_ids(pointers: list[str]) -> set[str]:
    """IDs referenced by one-line closed-bug pointers under ``## Fixed``."""
    return {m.group(1) for l in pointers if (m := POINTER_RE.match(l.strip()))}


def check(text: str) -> list[str]:
    head, entries, tail, strays, pointers = parse(text)
    problems: list[str] = []

    # closed-bug archive cross-check: every live pointer must resolve to a real
    # archived entry, every archived entry must have a live pointer, and an id
    # should never be a full entry in the live file AND already archived.
    arch_ids = archive_ids()
    if arch_ids or pointers:
        ptr_ids = pointer_ids(pointers)
        for bug_id in sorted(ptr_ids - arch_ids):
            problems.append(f"{bug_id}: has a ## Fixed pointer but no entry in {ARCHIVE.name}")
        for bug_id in sorted(arch_ids - ptr_ids):
            problems.append(f"{bug_id}: archived in {ARCHIVE.name} but has no ## Fixed pointer here")
        live_full_ids = {e.id for e in entries}
        for bug_id in sorted(arch_ids & live_full_ids):
            problems.append(f"{bug_id}: archived AND still a full entry in this file — stale duplicate")

    # per-entry status hygiene
    seen: dict[str, int] = {}
    for e in entries:
        seen[e.id] = seen.get(e.id, 0) + 1
        if e.declared_status is None:
            problems.append(f"{e.id}: no **Status: line (derives to {derive_status(e.heading)})")
        elif e.declared_status not in ALL_STATUSES:
            problems.append(f"{e.id}: unknown status '{e.declared_status}'")
        if e.status in RESOLVED and e.section == "Open":
            problems.append(f"{e.id}: status {e.status} but filed under ## Open (should be ## Fixed)")
        if e.status in ACTIVE and e.section == "Fixed":
            problems.append(f"{e.id}: status {e.status} but filed under ## Fixed (should be ## Open)")

    for bug_id, n in seen.items():
        if n > 1:
            problems.append(f"{bug_id}: id used by {n} distinct entries — renumber the collision")

    # index / entry cross-check
    entry_ids = {e.id for e in entries}
    active_ids = {e.id for e in entries if e.status not in RESOLVED}
    resolved_ids = {e.id for e in entries if e.status in RESOLVED}
    idx = index_ids(head)
    idx_set = set(idx)
    for bug_id in idx:
        if bug_id not in entry_ids:
            problems.append(f"{bug_id}: in the index but has no ### entry")
        elif bug_id in resolved_ids:
            problems.append(f"{bug_id}: resolved but still listed in the open-bug index")
    for bug_id in sorted(active_ids - idx_set):
        problems.append(f"{bug_id}: open but missing from the index table")

    # shipped-design orphans: an open bug whose named design has shipped
    shipped = shipped_designs()
    if shipped:
        for e in entries:
            if e.status in RESOLVED:
                continue
            waived = set(WAIVER_RE.findall("\n".join(e.lines)))
            for d in e.fix_designs:
                if d in shipped and d not in waived:
                    problems.append(
                        f"{e.id}: open, but names {d} which is SHIPPED — likely fixed, verify + mark")
                    break
    return problems


def shipped_designs() -> set[str]:
    """Names of *_DESIGN docs whose status line starts with SHIPPED (via design_status)."""
    try:
        sys.path.insert(0, str(HOOKS_DIR))
        import design_status  # noqa
    except Exception:
        return set()
    docs = REPO / "docs"
    out = set()
    for p in docs.glob("*_DESIGN.md"):
        try:
            sl = design_status.status_line(p)
        except Exception:
            sl = None
        if sl and sl.strip().upper().startswith("SHIPPED"):
            out.add(p.stem)  # e.g. DRAG_CAPTURE_DESIGN
    return out


def write(text: str) -> str:
    head, entries, tail, strays, pointers = parse(text)
    before = sorted((e.id, e.body_signature()) for e in entries)
    new_text = rebuild(head, entries, tail, pointers)
    # fidelity guard: reparse and prove no entry body content changed
    h2, e2, t2, _, p2 = parse(new_text)
    after = sorted((e.id, e.body_signature()) for e in e2)
    if [p.strip() for p in pointers] != [p.strip() for p in p2]:
        raise SystemExit(
            "FIDELITY GUARD TRIPPED — refusing to write: archive-pointer lines "
            f"changed across rebuild ({len(pointers)} before, {len(p2)} after).")
    if before != after:
        lost = {i for i, _ in before} - {i for i, _ in after}
        gained = {i for i, _ in after} - {i for i, _ in before}
        raise SystemExit(
            f"FIDELITY GUARD TRIPPED — refusing to write.\n"
            f"  entries before={len(before)} after={len(after)}\n"
            f"  lost={sorted(lost)} gained={sorted(gained)}\n"
            f"  (a body changed beyond the inserted **Status: line)")
    if strays:
        print(f"note: {len(strays)} stray non-entry line(s) were dropped from the sections:",
              file=sys.stderr)
        for s in strays:
            print(f"    {s!r}", file=sys.stderr)
    return new_text


def main() -> int:
    text = BACKLOG.read_text()
    if "--write" in sys.argv:
        # Main checkout has .git as a directory; worktrees have a .git file. Writing
        # the backlog in main violates the landing protocol (edits land via worktree
        # merges) — refuse here, at the write site, so the post-merge nudge can't be
        # followed literally in main. --force = deliberate human override.
        if (REPO / ".git").is_dir() and "--force" not in sys.argv:
            raise SystemExit(
                "refusing --write in the MAIN checkout (landing protocol: edits go "
                "through a worktree). Run the worktree's copy instead:\n"
                "  python3 .claude/worktrees/<name>/.claude/hooks/bug_status.py --write\n"
                "then land it as a --no-ff merge. Override (human, deliberate): --force")
        new_text = write(text)
        BACKLOG.write_text(new_text)
        moved = check(new_text)
        print(f"wrote {BACKLOG.relative_to(REPO)} — reflowed by **Status: line.")
        if moved:
            print("remaining drift (--check):")
            for p in moved:
                print(f"  · {p}")
        return 0
    problems = check(text)
    if not problems:
        print("bug-backlog status: clean")
        return 0
    print("⚠  bug-backlog status drift:", file=sys.stderr)
    for p in problems:
        print(f"  · {p}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
