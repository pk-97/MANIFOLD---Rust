#!/usr/bin/env python3
"""Design Status Board — single source of truth for design-doc status.

Reads the `**Status:` line from every docs/*_DESIGN.md and the date of the
last commit that touched the file, and prints a compact, grouped board.

The whole point: status lives in ONE place — the design doc's own status
line — and this board is GENERATED from it, never hand-copied. Memory files
must not restate design status; they point here. Because it reads straight
from the docs each run, it cannot drift: the moment a build session flips a
doc's status line, the next board reflects it.

Usage:
    python3 .claude/hooks/design_status.py          # print the board
    python3 .claude/hooks/design_status.py --raw    # one line per doc, untrimmed

The `last-changed` date is the drift check: a doc that says "not built" but
was touched this week is the flag to look closer (the Haiku merge housekeeper
automates that check; this is the human-readable view).
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOCS = REPO / "docs"
TRIM = 140  # max chars of the status line shown in grouped view

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


def build_board(raw: bool = False) -> str:
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
    width = max((len(n) for _, n, _, _ in rows), default=0)
    for b, label in enumerate(labels):
        group = sorted([r for r in rows if r[0] == b], key=lambda r: (r[2], r[1]), reverse=True)
        if not group:
            continue
        out.append(f"\n{label}")
        for _, name, date, status in group:
            text = status or "(no **Status line in doc)"
            if len(text) > TRIM:
                text = text[:TRIM - 1].rstrip() + "…"
            out.append(f"  {name:<{width}}  {date}  {text}")
    return "\n".join(out)


def main() -> int:
    print(build_board(raw="--raw" in sys.argv))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
