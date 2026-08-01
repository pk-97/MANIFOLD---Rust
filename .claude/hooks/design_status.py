#!/usr/bin/env python3
"""Design Status Board — single source of truth for design-doc status.

Reads the `**Status:` line from every docs/*_DESIGN.md and the date of the last commit
that touched the file, and prints a compact, grouped board. Status lives in ONE place —
the design doc's own status line — and this board is GENERATED from it, never
hand-copied. Memory files must not restate design status; they point here. Because it
reads straight from the docs each run, it cannot drift.

Usage:
    python3 .claude/hooks/design_status.py                    # print the board
    python3 .claude/hooks/design_status.py --raw              # one line per doc, untrimmed
    python3 .claude/hooks/design_status.py --lifecycle-check  # exit 1 on dead docs
    python3 .claude/hooks/design_status.py --dead-refs        # exit 1 on dead .rs refs

Lifecycle check: a SHIPPED design doc must either be cited by a live surface (CLAUDE.md,
hooks, memory, or any non-shipped doc — one hop, no credit for citations from other
shipped docs) or move to docs/archive/. Liveness is recomputed every run — nothing is
hand-marked. Override for a deliberate uncited keep: a `Lifecycle: contract` line in the
doc header. Enforced by crates/manifold-core/tests/docs_lifecycle.rs.

The `last-changed` date is the drift check: a doc that says "not built" but was touched
this week is the flag to look closer.

Header budget: a design doc's status header is state + owed/open items + one pointer,
capped at HEADER_CAP words. History, amendments, and per-phase stories live in the doc
body or beads. Docs still over budget are pinned at their current size in
`design_status_header_budget.txt` — any growth fails, and a doc that shrinks under the
cap must lose its pin (the ratchet only burns down). Enforced with the lifecycle check
via crates/manifold-core/tests/docs_lifecycle.rs.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOCS = REPO / "docs"
TRIM = 100  # max chars of the status line shown in grouped view

# Buckets in display order. First matching predicate wins, so order matters:
# check the "partial / in progress" signals before the plain "shipped" signal,
# because "P1-P3 SHIPPED; P4 remains" is in-progress, not done.
BUCKETS = [
    ("IN PROGRESS / PARTIAL", lambda s: "in progress" in s or "remain" in s
        or "partial" in s or "parked" in s or ("shipped" in s and "not built" in s)),
    ("PROPOSED - awaiting Peter", lambda s: "proposed" in s or "awaiting" in s),
    ("APPROVED - not built", lambda s: "not built" in s or "not implemented" in s),
    ("SHIPPED / BUILT", lambda s: "shipped" in s or "built" in s
        or "landed" in s or "done" in s or "code-complete" in s),
]


def status_line(path: Path) -> str | None:
    """First status line of a doc, cleaned. Matches both the bold `**Status:`
    form and a plain `Status:` line — several docs use the latter. Scans only the
    header region to avoid a body sentence that happens to start with "Status".
    None if the doc declares none."""
    for line in path.read_text(errors="replace").splitlines()[:40]:
        core = line.strip().lstrip("*#").lstrip()  # drop leading markdown
        if core[:6].lower() == "status" and (len(core) == 6 or core[6] in ":* "):
            text = core.replace("*", "").strip()
            if text[:6].lower() == "status":
                text = text[6:].lstrip(": ").strip()
            return " ".join(text.split())
    return None


def last_changed(path: Path) -> str:
    try:
        out = subprocess.run(
            ["git", "log", "-1", "--format=%ad", "--date=short", "--", str(path)],
            cwd=REPO, capture_output=True, text=True, timeout=5,
        )
        return out.stdout.strip() or "????-??-??"
    except Exception:
        return "????-??-??"


# Canonical tag → bucket index (into BUCKETS). The board keys on whichever tag
# appears FIRST in the doc's status line, so a line that leads "APPROVED design,
# not built · … the shipped async protocol …" buckets on the leading APPROVED,
# not the incidental "shipped" deeper in the sentence. Docs should lead with one
# of these words (DESIGN_DOC_STANDARD convention); the keyword fallback below
# only runs when a doc leads with none of them.
TAGS = [
    ("IN PROGRESS", 0), ("IN-PROGRESS", 0),
    ("PROPOSED", 1), ("AWAITING", 1),
    ("APPROVED", 2), ("NOT BUILT", 2), ("NOT IMPLEMENTED", 2),
    ("SHIPPED", 3), ("BUILT", 3), ("LANDED", 3), ("DONE", 3), ("CODE-COMPLETE", 3),
]


def bucket_of(status: str) -> int:
    head = status[:70].upper()
    found = sorted((head.find(tag), b) for tag, b in TAGS if tag in head)
    if found:
        return found[0][1]  # earliest canonical tag wins
    low = status.lower()  # no leading tag → best-effort keyword match
    for i, (_, pred) in enumerate(BUCKETS):
        if pred(low):
            return i
    return len(BUCKETS)  # falls into the "no clear status" tail


def _truncate_at_word(text: str, limit: int = 80) -> str:
    """Truncate text to ~limit chars at a word boundary with ellipsis."""
    if len(text) <= limit:
        return text
    truncated = text[:limit]
    last_space = truncated.rfind(" ")
    if last_space > 0:
        return truncated[:last_space] + " …"
    return truncated + "…"


def build_board(raw: bool = False, compact: bool = False) -> str:
    docs = sorted(DOCS.glob("*_DESIGN.md"))
    rows = []  # (bucket, name, date, status_or_None)
    for path in docs:
        name = path.stem.replace("_DESIGN", "")
        status = status_line(path)
        date = last_changed(path)
        b = bucket_of(status) if status else len(BUCKETS)
        rows.append((b, name, date, status))

    out: list[str] = []
    if raw:
        for _, name, date, status in sorted(rows, key=lambda r: r[1]):
            out.append(f"{date}  {name}: {status or '(no status line)'}")
        return "\n".join(out)

    out.append("DESIGN STATUS BOARD — generated from docs/*_DESIGN.md (the source of truth).")
    out.append("Regenerate: python3 .claude/hooks/design_status.py · never hand-copy status into memory.")
    labels = [b[0] for b in BUCKETS] + ["NO STATUS LINE - check the doc"]
    if compact:
        for b, label in enumerate(labels):
            group = sorted([r for r in rows if r[0] == b], key=lambda r: (r[2], r[1]), reverse=True)
            if not group:
                continue
            if b == 0:
                # IN PROGRESS / PARTIAL: show individual designs with truncated status
                out.append(f"\n{label}")
                width = max((len(n) for _, n, _, _ in group), default=0)
                for _, name, date, status in group:
                    text = status or "(no **Status line in doc)"
                    text = _truncate_at_word(text, 80)
                    out.append(f"  {name:<{width}}  {date}  {text}")
            else:
                # Other sections: one summary line
                out.append(f"{label}: {len(group)} design{'s' if len(group) > 1 else ''}"
                           f" — full board: python3 .claude/hooks/design_status.py")
        return "\n".join(out)

    # Full output (default, non-compact)
    width = max((len(n) for _, n, _, _ in rows), default=0)
    for b, label in enumerate(labels):
        group = sorted([r for r in rows if r[0] == b], key=lambda r: (r[2], r[1]), reverse=True)
        if not group:
            continue
        out.append(f"\n{label}")
        for _, name, date, status in group:
            if b == 3:
                # SHIPPED: the fact IS the status; the story lives in the doc.
                out.append(f"  {name:<{width}}  {date}")
                continue
            text = status or "(no **Status line in doc)"
            if len(text) > TRIM:
                # Status lines append their NEWEST facts at the END (2026-07-11:
                # head-only truncation hid a same-day "P8 SHIPPED" tail and a
                # session re-briefed already-landed work). Keep head AND tail.
                head = (TRIM * 2) // 5
                tail = TRIM - head - 3
                text = text[:head].rstrip() + " … " + text[-tail:].lstrip()
            out.append(f"  {name:<{width}}  {date}  {text}")
    return "\n".join(out)


# Index-like docs list every doc by name; a mention there is not a citation.
INDEX_DOCS = {"README.md", "DESIGN_BUILD_ORDER.md", "DESIGN_HARDENING_QUEUE.md"}


def dead_shipped_docs() -> list[str]:
    """SHIPPED design docs in docs/ top level with no live citation and no
    `Lifecycle: contract` override. These belong in docs/archive/."""
    docs = {p.name: p for p in DOCS.glob("*.md")}
    shipped = set()
    for p in DOCS.glob("*_DESIGN.md"):
        s = status_line(p)
        if s and bucket_of(s) == 3:
            shipped.add(p.name)
    live_text = (REPO / "CLAUDE.md").read_text(errors="replace")
    live_text += "".join(p.read_text(errors="replace")
                         for p in (REPO / ".claude/hooks").glob("*.py"))
    mem = Path.home() / ".claude" / "projects"
    live_text += "".join(p.read_text(errors="replace")
                         for p in mem.glob("*/memory/*.md"))
    live_text += "".join(docs[n].read_text(errors="replace") for n in docs
                         if n not in shipped and n not in INDEX_DOCS)
    dead = []
    for n in sorted(shipped):
        if n in live_text:
            continue
        header = "\n".join(docs[n].read_text(errors="replace").splitlines()[:40])
        if "lifecycle: contract" in header.lower():
            continue
        dead.append(n)
    return dead


HEADER_CAP = 120  # words; the contract for a healthy status header
BUDGET_FILE = Path(__file__).with_name("design_status_header_budget.txt")


def status_paragraph_words(path: Path) -> int:
    """Word count of the status header: every paragraph in the header region
    (before the first `---` or `##`) that starts with a status-family bold tag
    counts — the stacking disease appends sibling `**P5c …**` paragraphs, and a
    line-3-only count would miss them."""
    total, on = 0, False
    for line in path.read_text(errors="replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("##") or stripped == "---":
            break
        core = stripped.lstrip("*#").lstrip()
        if not on and core[:6].lower() == "status" and (len(core) == 6 or core[6] in ":* "):
            on = True
        elif on and not stripped:
            on = False
        elif not on and stripped.startswith("**") and any(
                core.startswith(t) for t in ("P", "SHIPPED", "LANDED", "AMENDED", "Wave")):
            on = True  # stacked sibling status paragraph
        if on:
            total += len(stripped.split())
    return total


def header_budget_failures() -> list[str]:
    """Ratchet lint. A doc over HEADER_CAP fails unless pinned at >= its size;
    a pinned doc that shrank under the cap fails until its pin is deleted."""
    pins: dict[str, int] = {}
    if BUDGET_FILE.exists():
        for line in BUDGET_FILE.read_text().splitlines():
            line = line.split("#")[0].strip()
            if line:
                name, words = line.rsplit(None, 1)
                pins[name] = int(words)
    fails = []
    for p in sorted(DOCS.glob("*_DESIGN.md")):
        words = status_paragraph_words(p)
        pin = pins.pop(p.name, None)
        if pin is not None and words <= HEADER_CAP:
            fails.append(f"HEADER {p.name}: {words} words — under cap; delete its pin "
                         f"from {BUDGET_FILE.name} (the ratchet only burns down)")
        elif pin is not None and words > pin:
            fails.append(f"HEADER {p.name}: {words} words, pinned at {pin} — headers "
                         "never grow; move the new prose to the body or beads")
        elif pin is None and words > HEADER_CAP:
            fails.append(f"HEADER {p.name}: {words} words > {HEADER_CAP} cap — the "
                         "status header is state + owed items + one pointer; history "
                         "goes to the body or beads")
    for name in pins:
        fails.append(f"HEADER {name}: pinned in {BUDGET_FILE.name} but no such doc — delete the pin")
    return fails


def lifecycle_check() -> int:
    dead = dead_shipped_docs()
    for n in dead:
        print(f"DEAD {n}: SHIPPED, cited by no live surface")
    header_fails = header_budget_failures()
    for f in header_fails:
        print(f)
    if dead:
        print(f"lifecycle: FAIL — {len(dead)} shipped doc(s) with no live citation. "
              "Either `git mv docs/<doc> docs/archive/` (+ scripts/gen_docs_index.py) "
              "or add a `Lifecycle: contract — <why>` header line.")
    if header_fails:
        print(f"lifecycle: FAIL — {len(header_fails)} status header(s) over budget "
              f"(cap {HEADER_CAP} words; ratchet file: {BUDGET_FILE.name}).")
    if dead or header_fails:
        return 1
    print("lifecycle: OK")
    return 0


def check_dead_refs() -> int:
    """Check for references to .rs files in docs/*.md that don't exist under crates/.

    Exit 1 if any dead references found in non-archived docs. This catches drift
    where docs reference deleted or renamed .rs files.
    """
    import re

    # Pattern to match .rs file references in markdown.
    # Matches: `foo.rs`, `path/to/foo.rs`, ../path/to/foo.rs, [text](path/to/foo.rs)
    RS_PATTERN = re.compile(r'(?:`|\[.*?\]\()?([a-zA-Z0-9_/-]+\.rs)(?:`|\))?')

    all_missing: list[tuple[str, str]] = []

    for doc_path in sorted(DOCS.glob("*.md")):
        # Skip archived docs
        if "archive" in doc_path.parts:
            continue

        content = doc_path.read_text(errors="replace")
        refs = RS_PATTERN.findall(content)

        for ref in refs:
            basename = Path(ref).name
            # Check if basename exists under crates/
            try:
                result = subprocess.run(
                    ["fd", "-q", f"^{basename}$", "crates/"],
                    cwd=REPO, capture_output=True, timeout=5,
                )
                if result.returncode != 0:
                    all_missing.append((f"{doc_path.name}:{ref}", basename))
            except Exception:
                pass

    if not all_missing:
        print("dead-refs: OK — no missing .rs file references in docs/*.md")
        return 0

    for doc_ref, basename in all_missing:
        print(f"DEAD-REF: docs/{doc_ref} (basename: {basename})")

    print(f"dead-refs: FAIL — {len(all_missing)} missing .rs reference(s) in docs/*.md")
    return 1


def main() -> int:
    if "--lifecycle-check" in sys.argv:
        return lifecycle_check()
    if "--dead-refs" in sys.argv:
        return check_dead_refs()
    compact = "--compact" in sys.argv
    board = build_board(raw="--raw" in sys.argv, compact=compact)
    if "--raw" not in sys.argv and not compact:
        dead = dead_shipped_docs()
        if dead:
            board += (f"\n\nARCHIVE CANDIDATES — shipped, cited by nothing live "
                      f"({len(dead)}): " + ", ".join(dead))
    print(board)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
